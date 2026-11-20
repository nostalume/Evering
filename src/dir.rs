//! Authors the index: the transaction word, the pending slab/entry/kind fields and the plain `Meta`
//! cell beside them, plus each record's state (generation and phase) and offset. Nothing here reaches a
//! caller — `pool`, `channel`, `session` and `header` are its whole audience — and only the winner of a
//! `claim` writes, through `persist`, `clear_pending`, `commit` and `restore` inside `create_in`,
//! `remove_in` and `grow`, or through `transfer` during recovery. `scan`, `offset` and `open_recorded`
//! read without claiming anything.
//!
//! The pending cell is plain memory, so the reads that decide an outcome have to follow the
//! transaction claim, and only atomic words may be inspected before it. The reason, the exceptions and
//! what a refusal hands back are spelled out in the doc on `recover` rather than repeated here.
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
pub(crate) const ID_BYTES: usize = 32;

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

#[cfg(test)]
std::thread_local! {
    /// The slot of a live participant whose heap mutation a test lets finish between the failed
    /// clear of a dead participant's mutation and the read that decides what that failure meant.
    static STAND_DOWN_WINDOW: core::cell::Cell<Option<u8>> = const { core::cell::Cell::new(None) };
}

/// Opens [`STAND_DOWN_WINDOW`] for the next `Directory::recover` on this thread.
#[cfg(test)]
pub(crate) fn stand_down_window_for_test(live: u8) {
    STAND_DOWN_WINDOW.with(|window| window.set(Some(live)));
}

#[cfg(test)]
fn stand_down(heap: &crate::talc::MapTalc) {
    if let Some(live) = STAND_DOWN_WINDOW.with(|window| window.take()) {
        assert!(
            heap.clear_dead_owner(live),
            "the mutation the window was armed for is already gone"
        );
    }
}

#[cfg(test)]
static RESUME_WINDOW: AtomicUsize = AtomicUsize::new(0);

/// Opens the window between a resumer's read of the transaction word and the heap mutation it waits
/// on, so the first thread through `resume_rollback` stops there and the next one is the resumer that
/// would finish the rollback while the first is away. 0 closed, 1 open, 2 parked, 3 released.
#[cfg(test)]
pub(crate) fn open_resume_window_for_test() {
    RESUME_WINDOW.store(1, Ordering::Release);
}

#[cfg(test)]
pub(crate) fn resume_window_parked_for_test() -> bool {
    RESUME_WINDOW.load(Ordering::Acquire) == 2
}

#[cfg(test)]
pub(crate) fn close_resume_window_for_test() {
    RESUME_WINDOW.store(3, Ordering::Release);
}

#[cfg(test)]
fn park_in_resume_window() {
    // One-shot on purpose: only the first thread through the window stops.
    if RESUME_WINDOW
        .compare_exchange(1, 2, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    while RESUME_WINDOW.load(Ordering::Acquire) == 2 {
        std::thread::yield_now();
    }
}

#[cfg(test)]
static REAP_TARGET: AtomicUsize = AtomicUsize::new(usize::MAX);
#[cfg(test)]
static REAP_GATE: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static REAP_HELD: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static REAP_READS: AtomicUsize = AtomicUsize::new(0);

/// Two stop points around the claim in `recover`'s `REMOVE` arm, which together put one reaper between
/// the entry gates and the claim and a second one behind the claim with the cell already read. The
/// `dead` slot names which reap stops: the windows are process-wide and other tests reap `REMOVE`
/// transactions of their own, and a reaper of another test that walked into an open window would wait
/// for a release nobody there intends to make. 0 closed, 1 open, 2 parked, 3 released.
#[cfg(test)]
pub(crate) fn open_reap_windows_for_test(dead: u8) {
    REAP_READS.store(0, Ordering::Relaxed);
    REAP_GATE.store(0, Ordering::Relaxed);
    REAP_HELD.store(0, Ordering::Relaxed);
    REAP_TARGET.store(usize::from(dead), Ordering::Release);
    REAP_GATE.store(1, Ordering::Release);
    REAP_HELD.store(1, Ordering::Release);
}

#[cfg(test)]
pub(crate) fn reap_gate_parked_for_test() -> bool {
    REAP_GATE.load(Ordering::Acquire) == 2
}

#[cfg(test)]
pub(crate) fn release_reap_gate_for_test() {
    REAP_GATE.store(3, Ordering::Release);
}

#[cfg(test)]
pub(crate) fn reap_held_parked_for_test() -> bool {
    REAP_HELD.load(Ordering::Acquire) == 2
}

#[cfg(test)]
pub(crate) fn release_reap_held_for_test() {
    REAP_TARGET.store(usize::MAX, Ordering::Release);
    REAP_HELD.store(3, Ordering::Release);
}

#[cfg(test)]
fn reap_window_applies(dead: u8) -> bool {
    REAP_TARGET.load(Ordering::Acquire) == usize::from(dead)
}

#[cfg(test)]
fn park_at_reap_gate(dead: u8) {
    // One-shot on purpose: only the first matching thread through the window stops.
    if !reap_window_applies(dead)
        || REAP_GATE
            .compare_exchange(1, 2, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        return;
    }
    while REAP_GATE.load(Ordering::Acquire) == 2 {
        std::thread::yield_now();
    }
}

#[cfg(test)]
fn park_at_reap_held(dead: u8) {
    if !reap_window_applies(dead)
        || REAP_HELD
            .compare_exchange(1, 2, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        return;
    }
    while REAP_HELD.load(Ordering::Acquire) == 2 {
        std::thread::yield_now();
    }
}

/// Counts this reap's read of the staged span and stops the reader that holds the claim, so a test can
/// look at the arm from the other side while the cell is claimed.
#[cfg(test)]
fn note_reap_read(dead: u8) {
    if reap_window_applies(dead) {
        REAP_READS.fetch_add(1, Ordering::Relaxed);
        park_at_reap_held(dead);
    }
}

/// Takes the number of reads `recover`'s `REMOVE` arm has made of the staged span for the armed reap.
/// The ordering the claim buys is invisible from a verdict — a reaper that lost the CAS answers
/// `Busy` either way, because the claim it tried to take is what names the holder — so what a test has
/// to observe is whether it looked at the cell on the way to that answer.
#[cfg(test)]
pub(crate) fn take_reap_reads_for_test() -> usize {
    REAP_READS.swap(0, Ordering::AcqRel)
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

/// Which refusal stopped an open, named at the point that refused.
///
/// Opening a recorded layout reads the directory entry, then the record's own header word, then the
/// header's admission of this member. Those fail for different reasons and a caller handles them
/// differently: `Busy` is worth retrying, `OutOfBounds` and `InvalidHeader` mean the region is
/// telling us something it should not be, and `Absent` means it is telling the truth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpenCause {
    WrongRegion,
    Absent,
    OutOfBounds,
    Misaligned,
    ReadOnly,
    InvalidHeader,
    Mismatch(crate::header::LayoutField),
    Undiscoverable,
    Closed,
    DuplicateMember,
    Busy,
}

impl OpenCause {
    fn projection(error: crate::mem::ProjectionError) -> Self {
        match error {
            crate::mem::ProjectionError::ReadOnly => Self::ReadOnly,
            crate::mem::ProjectionError::ArithmeticOverflow
            | crate::mem::ProjectionError::OutOfBounds => Self::OutOfBounds,
            crate::mem::ProjectionError::Misaligned => Self::Misaligned,
            crate::mem::ProjectionError::Admission(error) => Self::admission(error),
        }
    }

    fn admission(error: crate::header::AdmitError) -> Self {
        match error {
            crate::header::AdmitError::Contention => Self::Busy,
            crate::header::AdmitError::Closed => Self::Closed,
            crate::header::AdmitError::DuplicateMember => Self::DuplicateMember,
            crate::header::AdmitError::InvalidHeader => Self::InvalidHeader,
            crate::header::AdmitError::Mismatch(field) => Self::Mismatch(field),
        }
    }
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
                    // An entry between states: the walk cannot promise an enumeration of a directory
                    // whose transaction is still deciding this member, so `InUse` here means "a
                    // transaction is open at this entry", not "the slot is taken".
                    // `Session::scan_recovery` discards it because every handler answer to it is the
                    // same one: reap again, or hand the authority to a participant that holds the
                    // transaction.
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
    ) -> Result<(crate::mem::Mapped<header::RcHeader<L>>, U), OpenCause> {
        let offset = self.offset(id).map_err(|_| {
            if id.region != self.region_id() {
                OpenCause::WrongRegion
            } else {
                OpenCause::Absent
            }
        })?;
        let record =
            unsafe { self.ref_at::<header::Record>(offset) }.map_err(OpenCause::projection)?;
        let recorded = unsafe { header::recorded_at(core::ptr::from_ref(record).cast()) }
            .map_err(OpenCause::admission)?;
        self.offset_of(core::ptr::NonNull::from(record).cast(), recorded.extent)
            .map_err(OpenCause::projection)?;
        let header =
            unsafe { self.ref_at::<header::RcHeader<L>>(offset) }.map_err(OpenCause::projection)?;
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
        .map_err(OpenCause::admission)?;
        let (conf, value) = discover(info, recorded.extent).ok_or(OpenCause::Undiscoverable)?;
        let mapped = self
            .admit_at::<header::RcHeader<L>>(offset, conf, false)
            .map_err(OpenCause::projection)?;
        Ok((mapped, value))
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

    /// Refuses a staged block span this region cannot reach.
    ///
    /// `pending` holds whatever the participant that last owned the transaction word left there, and
    /// recovery dereferences it: it reads the member's closed flag at that span and hands the span to
    /// `deallocate`, which reads a block header just below it. The span is therefore peer input, and it
    /// has to lie inside the region and sit at an address the allocator can use before either of those
    /// happens — the same test `recover_growth` runs on the slab its peer staged.
    fn staged_span_reachable(
        &self,
        heap: &crate::talc::MapTalc,
        meta: &crate::talc::Meta,
    ) -> Result<(), Error> {
        let pointer = heap.pointer(meta);
        let block = core::mem::size_of::<header::RcHeader<()>>();
        self.offset_of(pointer, block.max(meta.layout().size()))
            .map_err(Error::Admission)?;
        if !crate::talc::block_aligned(pointer) {
            return Err(Error::Admission(crate::mem::ProjectionError::Misaligned));
        }
        Ok(())
    }

    /// Adjudicates what a dead participant left in the shared transaction.
    ///
    /// The reads that decide the outcome — `pending`, the member's closed flag, the heap's mutation
    /// state, the entry's own state word — are taken after `transfer` claims the transaction word. That
    /// order is what makes the non-atomic cell trustworthy: the writer claims before it touches the cell
    /// (`claim`, then `Transaction::persist`), so a reader that holds the claim cannot overlap a writer,
    /// and the value it decides on cannot be torn underneath it. `recover_growth` and the `REMOVE` and
    /// `CREATE` arms all claim first; what may still be inspected before the claim is an atomic word —
    /// the heap's owner and the entry's own state — and a compare-exchange on one of those cannot be
    /// half-applied, so the worst a stale read of those can do is name the wrong holder, which is what
    /// the stand-down re-read answers.
    ///
    /// A claim that decides nothing then has one way to be dropped and one way to be closed. A refusal —
    /// transient or permanent — uses `Transaction::restore` to hand back the word exactly as it was read,
    /// because `recover` returns `Stale` on a clean word and a clean word erases the only evidence that
    /// this member has work outstanding; the caller is told the cause on the same edge, and a later reap
    /// either finishes the work or reports the same refusal. `CLEAN` is written only with a verdict: the
    /// successful stores, and `quarantine_held` for a poisoned heap. Nothing in this function leaves a
    /// claim to `Drop`'s demotion: that path marks a participant abandoning its own interrupted
    /// transaction, and there is no `REMOVE` resume arm that could finish one left on a live slot.
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
            // The claim is taken before anything that reads `pending` or the member's closed flag. That
            // is what makes the reads whole: the writer takes the claim before it touches the cell
            // (`claim` then `Transaction::persist`), so holding it excludes every writer and every other
            // reaper. `clear_dead_owner` moved behind the claim too, which costs nothing here and buys
            // one thing: a reaper that lost the CAS no longer rewrites the dead participant's heap word.
            #[cfg(test)]
            park_at_reap_gate(dead);
            let mut transaction = self.transfer(word, dead, phase)?;
            if !heap.clear_dead_owner(dead) {
                #[cfg(test)]
                stand_down(heap);
                return match heap.mutation_state() {
                    // Every arm below is a refusal of a condition that can change, so the claim goes
                    // back as it was read instead of being committed: `recover` will not enter on a
                    // clean word, and a clean word is the only record that this member still has a
                    // `REMOVE` outstanding.
                    MutationState::Owned(owner) => {
                        // The result is ignored on purpose: only a peer scribbling the word can make
                        // this CAS fail, and re-validating a word only an honest peer writes is
                        // outside the stage (delta 19).
                        let _ = transaction.restore(word);
                        Err(Error::Busy(owner))
                    }
                    MutationState::Poisoned => Ok(self.quarantine_held(entry, transaction)),
                    // `clear_dead_owner` counts an already-clean heap as success, so a false
                    // return that reads back as `Clean` means the word moved to clean inside that
                    // window: a live participant finished a mutation of its own, and this reaper
                    // never saw the state it tried to change. Standing down fails this reap and
                    // hands the handle back, so the next one reads a heap it actually observed
                    // instead of clearing or poisoning a mutation someone still running owns.
                    MutationState::Clean => {
                        let _ = transaction.restore(word);
                        Err(Error::Stale)
                    }
                };
            }
            let meta = *transaction.pending();
            #[cfg(test)]
            note_reap_read(dead);
            if let Err(error) = self.staged_span_reachable(heap, &meta) {
                // Refusing a span the same way as before the reorder — evidence kept, word as read — is
                // what `recovery_refuses_a_staged_span_outside_the_region` pins: the member stays
                // `CLOSING` for a participant that can adjudicate it, and committing here would erase the
                // only record that this reap had anything left to do. The copy above is what the free
                // below uses, because the claim now guarantees nobody wrote the cell in between.
                let _ = transaction.restore(word);
                return Err(error);
            }
            if !unsafe { header::closed_at(heap.pointer(&meta).as_ptr()) } {
                entry
                    .state
                    .store((generation << STATE_BITS) | PUBLISHED, Ordering::Release);
                transaction.clear_pending();
                transaction.commit();
                return Ok(Recovered::RolledBack);
            }
            let mutation = match heap.begin_mutation() {
                Ok(mutation) => mutation,
                Err(error) => {
                    let _ = transaction.restore(word);
                    return Err(Error::Heap(error));
                }
            };
            if self.inner.transaction.load(Ordering::Acquire)
                != Directory::owner_word(transaction.owner, transaction.phase)
            {
                // A resumer of this same slot took the heap first. Its filter is a value check, not a
                // claim — the transaction already names this participant, so a CAS onto the same value
                // succeeds for both threads and only this re-read, on the far side of the lock the
                // resumer had to wait for, tells them apart. Dropping the claim here is deliberate: the
                // demotion CAS can only fail, because the word is no longer this thread's to move.
                mutation.commit();
                return Err(Error::Stale);
            }
            // The copy taken under the claim is what the free below uses, because the re-check above
            // proves nobody has been able to write the cell since: recovery frees what it checked.
            unsafe {
                heap.deallocate_during(&mutation, heap.pointer(&meta), meta.layout());
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
                #[cfg(test)]
                stand_down(heap);
                return match heap.mutation_state() {
                    MutationState::Owned(owner) => Err(Error::Busy(owner)),
                    MutationState::Poisoned => self.quarantine(word, entry),
                    MutationState::Clean => Err(Error::Stale),
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
                let meta = *transaction.pending();
                if let Err(error) = self.staged_span_reachable(heap, &meta) {
                    mutation.commit();
                    transaction.commit();
                    return Err(error);
                }
                unsafe {
                    heap.deallocate_during(&mutation, heap.pointer(&meta), meta.layout());
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
                let meta = *transaction.pending();
                if let Err(error) = self.staged_span_reachable(heap, &meta) {
                    mutation.commit();
                    transaction.commit();
                    return Err(error);
                }
                unsafe {
                    heap.deallocate_during(&mutation, heap.pointer(&meta), meta.layout());
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

    /// Reclaims a transaction whose participant died while growing the slab chain.
    ///
    /// The claim is taken before anything that needs `pending`, which is what makes the cell's contents
    /// trustworthy here: only a holder of the transaction word writes it. The cost is that every exit
    /// below the claim leaves the word held by this participant, and `GROW` can pay it because
    /// `Transaction`'s drop demotes such a claim to `ROLLING_BACK` and `create_in` hands its own
    /// `ROLLING_BACK` straight back here — a reaper that bails on a bad pointer retries rather than
    /// stranding the record. `recover`'s REMOVE arm cannot take the same shape, because
    /// `resume_rollback` answers `Busy` for `REMOVE` and an exit below that claim has nowhere to finish.
    ///
    /// Two threads of one participant can both hold this claim, since resuming one's own transaction
    /// CASes the word onto the value it already has. The `Clean` arm therefore re-reads the word under
    /// `begin_mutation` — the thing those two actually contend for — and the late one stands down.
    fn recover_growth(
        &self,
        heap: &crate::talc::MapTalc,
        owner: u8,
        word: usize,
        phase: usize,
        dead: bool,
    ) -> Result<Recovered, Error> {
        use crate::talc::MutationState;

        let mut transaction = self.transfer(word, owner, phase)?;
        let slab_index = u32::try_from(self.inner.pending_slab.load(Ordering::Relaxed))
            .map_err(|_| Error::Stale)?;
        let predecessor = slab_index.checked_sub(1).ok_or(Error::Stale)?;
        let meta = *transaction.pending();
        let pointer = heap.pointer(&meta);
        let offset = self
            .offset_of(pointer, core::mem::size_of::<Slab>())
            .map_err(Error::Admission)?;
        if !crate::talc::block_aligned(pointer) {
            return Err(Error::Admission(crate::mem::ProjectionError::Misaligned));
        }
        let link = self.slab(predecessor)?.next().load(Ordering::Acquire);

        if link == offset {
            // The peer's own store won: the slab it staged is the one the chain already points at, so
            // the chain is whole and only the claim has to be let go.
            transaction.clear_pending();
            transaction.commit();
            return Ok(Recovered::Retained);
        }
        if link != 0 {
            // Another slab owns the link, so the staged span is unreachable from the chain: not the
            // tail's successor, and not freeable either, since nothing here can prove what it holds.
            transaction.clear_pending();
            transaction.commit();
            return Ok(Recovered::Quarantined);
        }

        if dead && matches!(heap.mutation_state(), MutationState::Owned(found) if found == owner) {
            let _ = heap.poison_owner(owner);
        }
        match heap.mutation_state() {
            MutationState::Clean => {
                let mutation = heap.begin_mutation().map_err(Error::Heap)?;
                if self.inner.transaction.load(Ordering::Acquire)
                    != Directory::owner_word(transaction.owner, transaction.phase)
                {
                    mutation.commit();
                    return Err(Error::Stale);
                }
                unsafe {
                    heap.deallocate_during(&mutation, pointer, core::alloc::Layout::new::<Slab>())
                };
                // `clear_pending` before the heap goes back: it moves the word off the value the other
                // resumer re-reads, so the window between releasing the heap and committing the
                // transaction cannot be entered as two winners.
                transaction.clear_pending();
                mutation.commit();
                transaction.commit();
                Ok(Recovered::RolledBack)
            }
            MutationState::Owned(found) => Err(Error::Busy(found)),
            MutationState::Poisoned => {
                transaction.clear_pending();
                transaction.commit();
                Ok(Recovered::Quarantined)
            }
        }
    }

    /// Claims the transaction word from the exact value `word` the caller read.
    ///
    /// `Busy` names whoever holds the transaction now, which is the only thing a losing reaper can
    /// honestly say about a word it lost. A current word that names nobody is not that: the
    /// transaction went clean inside this window, so somebody finished what this reaper came to do
    /// and no participant holds anything. `Stale` is the arm for that, the same answer `recover`
    /// gives at its own entry for the same condition — `Stale` means "there is nothing left here for
    /// you to recover", not "somebody is busy". Reporting `dead` instead, as this arm did, asserted
    /// that a participant that just stopped existing still holds the transaction.
    fn transfer(&self, word: usize, dead: u8, phase: usize) -> Result<Transaction<'_>, Error> {
        // Every caller reaches here with a word it read for the participant it came to recover from,
        // and the CAS below is only meaningful against that word. `resume_rollback`'s arm passes its
        // own slot as `dead`, which is the case where the resumer is finishing its own transaction.
        debug_assert_eq!(Directory::word_owner(word), Some(dead));
        let live = self.peer().slot();
        self.inner
            .transaction
            .compare_exchange(
                word,
                Directory::owner_word(live, phase),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|current| match Directory::word_owner(current) {
                Some(owner) => Error::Busy(owner),
                None => Error::Stale,
            })?;
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
        Ok(self.quarantine_held(entry, held))
    }

    /// The same verdict for a reaper that already holds the claim: the store and the commit, with no
    /// second `transfer`, which against the word this transaction moved could only fail.
    fn quarantine_held(&self, entry: &Entry, transaction: Transaction<'_>) -> Recovered {
        let generation = entry.state.load(Ordering::Acquire) >> STATE_BITS;
        entry
            .state
            .store((generation << STATE_BITS) | QUARANTINED, Ordering::Release);
        debug_assert!(transaction.commit());
        Recovered::Quarantined
    }

    /// Finishes a `ROLLING_BACK` transaction its own owner left behind.
    ///
    /// The check at the top is only a filter: two threads of one participant both pass it, because a
    /// resumer claims no word — the transaction already names this participant, so a CAS onto the
    /// same value would succeed for both. The authority is the re-check below, taken with the heap
    /// mutation held: `begin_mutation` serializes the resumers and the second one then sees the word
    /// the first committed and stands down. Without that re-check the second reads `pending` again
    /// and frees the span the first already returned to the free list; the two-resumer test in
    /// `src/tests/mock.rs` fails on exactly that, and `talc.rs:1180` is what catches it in a debug
    /// build.
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
        let mutation = {
            #[cfg(test)]
            park_in_resume_window();
            heap.begin_mutation().map_err(Error::Heap)?
        };
        if self.inner.transaction.load(Ordering::Acquire) != expected {
            // A second resumer of one `ROLLING_BACK` transaction reaches here while the first still
            // holds the heap; once it does not, the first has finished. `clear_pending` rewrites the
            // transaction word and leaves the staged span in the cell, so reading `pending` again
            // would free a block the winner already returned to the free list. The heap goes back
            // untouched and no transaction handle is fabricated over a word nobody holds.
            mutation.commit();
            return Err(Error::Stale);
        }
        let mut transaction = Transaction {
            directory: &self.inner,
            owner,
            phase: ROLLING_BACK,
        };
        let meta = *transaction.pending();
        if let Err(error) = self.staged_span_reachable(heap, &meta) {
            mutation.commit();
            transaction.commit();
            return Err(error);
        }
        let pointer = heap.pointer(&meta);
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

    /// Overwrites the block span a staged transaction names. A participant that dies holding the
    /// transaction word leaves that field to whatever it last wrote, so recovery has to treat it as
    /// peer-supplied; this lets a test leave something no allocator would have produced.
    #[cfg(test)]
    pub(crate) fn forge_pending_span_for_test(&self, meta: crate::talc::Meta) {
        unsafe { (*self.inner.pending.get()).write(meta) };
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

    /// Stages a create `owner` was given a block for and never finished, in the phase `phase`.
    #[cfg(test)]
    fn abandon_create_in(&self, owner: u8, phase: usize, meta: crate::talc::Meta) {
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
            .store(Directory::owner_word(owner, phase), Ordering::Release);
    }

    #[cfg(test)]
    pub(crate) fn abandon_allocated_create_for_test(&self, dead: u8, meta: crate::talc::Meta) {
        self.abandon_create_in(dead, EVIDENCE, meta);
    }

    /// Stages a create owned by a live participant that reached the phase only its own owner resumes,
    /// so a test can race two of that participant's threads through `resume_rollback`.
    #[cfg(test)]
    pub(crate) fn abandon_rolling_back_create_for_test(&self, owner: u8, meta: crate::talc::Meta) {
        self.abandon_create_in(owner, ROLLING_BACK, meta);
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

    pub(crate) fn to_bytes(self) -> [u8; ID_BYTES] {
        let mut bytes = [0; ID_BYTES];
        bytes[..8].copy_from_slice(&self.region.high.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.region.low.to_le_bytes());
        bytes[16..20].copy_from_slice(&self.slab.to_le_bytes());
        bytes[20..24].copy_from_slice(&self.entry.to_le_bytes());
        bytes[24..].copy_from_slice(&(self.generation as u64).to_le_bytes());
        bytes
    }

    pub(crate) fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != ID_BYTES {
            return None;
        }
        Some(Self::from_parts(
            RegionId::new(
                u64::from_le_bytes(bytes[..8].try_into().unwrap()),
                u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            ),
            u32::from_le_bytes(bytes[16..20].try_into().unwrap()),
            u32::from_le_bytes(bytes[20..24].try_into().unwrap()),
            usize::try_from(u64::from_le_bytes(bytes[24..].try_into().unwrap())).ok()?,
        ))
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

// Safety: every field but `pending` is an atomic or an immutable-after-admission entry, so a shared
// `&Directory` never races on them. `pending` is plain non-atomic memory: the transaction word is
// the lock that gates its stores, which happen only through `Transaction::persist` and
// `clear_pending`, and every read of the cell — the live path's and every recovery arm's, including
// `recover`'s `REMOVE` — is taken holding that claim, which is the rule stated on `recover`.
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

    /// Hands the transaction word back the exact value a reaper read before it claimed, and is the bail
    /// for a claim that decided nothing.
    ///
    /// `commit` writes `CLEAN`, which belongs to a verdict: the successful edges end in `clear_pending`
    /// and then `commit`, and so does `quarantine_held`, whose store says what the member has become.
    /// Neither is right for a refusal. `recover` returns `Stale` on a clean word, so committing to
    /// `CLEAN` on a refusal erases the only record that this member still has a `REMOVE` outstanding —
    /// and that holds whether the refused condition can change (a live mutation that has not finished) or
    /// cannot (a span that does not resolve inside the region), because in both cases the next reap is
    /// what reports it. Nor is `Drop`'s demotion to `ROLLING_BACK` right: that marks a participant
    /// abandoning its *own* transaction, and a `REMOVE` claim left on a live slot has no way to be
    /// finished, since `resume_rollback` answers `Busy` for `REMOVE` and every other reap of that
    /// participant is refused by the owner check at `recover`'s entry. Restoring the observed word puts
    /// the cell back exactly as this reaper found it, which is what makes the next reap see the same
    /// condition and, when that condition was a live mutation that has since finished, act on it. Only
    /// this reaper can have moved the word, because the claim it holds is the only door to it.
    fn restore(self, observed: usize) -> bool {
        let restored = self
            .directory
            .transaction
            .compare_exchange(
                Directory::owner_word(self.owner, self.phase),
                observed,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        core::mem::forget(self);
        restored
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
