//! Owns one authority word per block slot — generation, allocated bit, owning member — with the
//! payload bytes it hands out and the class geometry it records in its own header. Only `claim`'s
//! compare-exchange attaches a slot to a member, `detach` and `takeover` move it, and `release` hands
//! it back; the directory word that says what a block *is* belongs to `dir`, which this module reads
//! and never writes.
//!
//! A reserve answer rests on the control word alone, so a word naming a state no participant writes
//! has to return as `Corrupted` rather than `NoRoom { waits: Release }`: read as a shortage, one
//! scribbled word makes a healthy region look permanently full, and every retry after it is then
//! correct.
use alloc::{boxed::Box, vec::Vec};
use core::{
    alloc::Layout as AllocLayout,
    marker::PhantomData,
    mem::{ManuallyDrop, MaybeUninit},
    ops::Deref,
    ptr::{self, NonNull},
    sync::atomic::{AtomicU32, Ordering},
};

use crate::{
    dir,
    header::{self, RcHeader, RecoveryContext},
    mem::{Decline, DeclineCause, Mapped, RoomWait},
    msg::Repr,
    place::LayoutContext,
    schema::{LayoutInfo, SchemaKey, SharedSchema, schema_id},
    talc::MapTalc,
    token::{ClassSlot, Shape, Token},
};

const OWNER_BITS: u32 = 6;
const OWNER_MASK: u32 = (1 << OWNER_BITS) - 1;
const ALLOCATED: u32 = 1 << OWNER_BITS;
const GENERATION_SHIFT: u32 = OWNER_BITS + 1;
const MAX_GENERATION: u32 = u32::MAX >> GENERATION_SHIFT;
const MIN_BLOCK: usize = 64;
const DEFAULT_MAX: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
struct State(u32);

impl State {
    const fn free(generation: u32) -> Self {
        Self(generation << GENERATION_SHIFT)
    }

    const fn local(generation: u32, owner: u8) -> Option<Self> {
        if generation > MAX_GENERATION || owner as usize >= crate::header::PARTICIPANT_CAPACITY {
            return None;
        }
        Some(Self(
            (generation << GENERATION_SHIFT) | ALLOCATED | (owner as u32 + 1),
        ))
    }

    const fn detached(generation: u32) -> Self {
        Self((generation << GENERATION_SHIFT) | ALLOCATED)
    }

    const fn generation(self) -> u32 {
        self.0 >> GENERATION_SHIFT
    }

    const fn owner(self) -> Option<u8> {
        let owner = self.0 & OWNER_MASK;
        if owner == 0 {
            None
        } else {
            Some((owner - 1) as u8)
        }
    }

    const fn allocated(self) -> bool {
        self.0 & ALLOCATED != 0
    }

    const fn canonical(self) -> bool {
        self.allocated() || self.owner().is_none()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockRange {
    min: usize,
    max: usize,
}

impl BlockRange {
    pub fn new(min: usize, max: usize) -> Result<Self, RangeError> {
        let min = min
            .max(MIN_BLOCK)
            .checked_next_power_of_two()
            .ok_or(RangeError)?;
        let max = max
            .max(MIN_BLOCK)
            .checked_next_power_of_two()
            .ok_or(RangeError)?;
        (min <= max).then_some(Self { min, max }).ok_or(RangeError)
    }

    pub const fn min(self) -> usize {
        self.min
    }

    pub const fn max(self) -> usize {
        self.max
    }

    /// The smallest extent that can hold a pool whose classes are exactly this range.
    ///
    /// Only a named range has a floor: the `None` a caller may instead hand to
    /// [`Session::create_pool`](crate::Session::create_pool) shrinks its class count until the extent it
    /// was given covers it, and would report a number its own call never refuses at.
    pub fn required_extent(self) -> Result<usize, PoolCreateError> {
        let count = self.max.ilog2() - self.min.ilog2() + 1;
        Geometry::for_count(self, count).map(|geometry| geometry.layout.size())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RangeError;

impl core::fmt::Display for RangeError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("no run of power-of-two block classes fits between these bounds")
    }
}

#[cfg(feature = "std")]
impl std::error::Error for RangeError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolCreateError {
    InvalidExtent,
    RequiredExtent {
        required: usize,
        available: usize,
    },
    /// The widest class needs more alignment than every mapping of a region is guaranteed to have,
    /// so a peer could not reproduce the block addresses this Pool hands out.
    ClassTooWide(usize),
    /// The shared heap, this mapping, or a member's transaction refused the pool's own record, in the
    /// cause that refusal gave.
    Declined(crate::mem::DeclineCause),
    /// The directory refused the record for its own reason — a stale id, a publish recovery undid, words
    /// in the region that disagree — which no release, retry or wider region answers.
    Record,
}

impl core::fmt::Display for PoolCreateError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidExtent => {
                f.write_str("the block range cannot be built into a pool layout")
            }
            Self::RequiredExtent {
                required,
                available,
            } => write!(
                f,
                "the classes reserve {required} bytes before any payload arrives; {available} were offered"
            ),
            Self::ClassTooWide(bytes) => write!(
                f,
                "a {bytes}-byte class needs an alignment a peer's mapping is not guaranteed to reproduce"
            ),
            Self::Declined(cause) => {
                write!(f, "the pool's own record was refused: {cause}")
            }
            Self::Record => f.write_str(
                "the directory refused the pool's own record for a reason no retry answers",
            ),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for PoolCreateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Declined(cause) => Some(cause),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AdoptError {
    Pool,
    NoSlot,
    Type,
    Owned,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Info {
    min: u64,
    base: u64,
    classes: u64,
}

impl SharedSchema for Info {
    const SCHEMA: SchemaKey = SchemaKey::new(schema_id("evering.pool.info"), 1);
}

unsafe impl LayoutInfo for Info {}

#[derive(Clone, Copy)]
pub(crate) struct Config {
    info: Info,
    layout: AllocLayout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Class {
    bytes: usize,
    slots: usize,
    controls: usize,
    payload: usize,
}

impl Class {
    /// The address of `slot`'s authority word: a location inside the region rather than one of
    /// this struct's fields, since the control array is laid out by [`Geometry`] and reached from
    /// the base its mapping's owner holds. This is the only spelling of that arithmetic.
    ///
    /// [`PoolRef::control`], [`RecoveryContext::control`] and [`RcHeader::control`] are what turn
    /// the address into a borrow, and each states the claim that bounds how long the word may be
    /// used. [`Storage::init`] is the only writer of a raw one, while no word yet holds a state.
    fn control(self, base: *const u8, slot: usize) -> *const AtomicU32 {
        base.wrapping_add(self.controls)
            .cast::<AtomicU32>()
            .wrapping_add(slot)
    }

    fn payload(self, base: *mut u8, slot: usize) -> *mut u8 {
        base.wrapping_add(self.payload + slot * self.bytes)
    }
}

/// The authority words of a region settled by a reap, which reaches the pool as a
/// [`RecoveryContext`] rather than as a live handle.
impl RecoveryContext<'_, Storage> {
    /// `slot`'s word in the region this context names.
    ///
    /// Settling a dead participant's ownership holds the mapping only for the call, so the borrow
    /// is the context's and not a lifetime a caller picks: a word restated here cannot be carried
    /// out of the reap that restated it.
    fn control(&self, class: Class, slot: usize) -> &AtomicU32 {
        unsafe { &*class.control(self.base, slot) }
    }
}

/// The authority words of a region named by the header that fronts it, which is how the transfer
/// arms reach a pool they hold no handle for.
impl RcHeader<Storage> {
    /// `slot`'s word in the region whose header this is.
    ///
    /// A control array begins at the region's base, and the region's base is where its header
    /// sits, so the header's own borrow bounds the word — the same proof [`Storage::recover`] owes
    /// when it holds a [`RecoveryContext`] instead.
    fn control(&self, class: Class, slot: usize) -> &AtomicU32 {
        unsafe { &*class.control(core::ptr::from_ref(self).cast(), slot) }
    }
}

struct Geometry {
    layout: AllocLayout,
    classes: Box<[Class]>,
}

impl Geometry {
    /// The geometry of `range` spread over `count` classes, each keeping the slots an exact range
    /// reserves before any payload arrives.
    ///
    /// This is the one computation behind both answers a caller can get: the number
    /// [`BlockRange::required_extent`] reports and the one `RequiredExtent` refuses with, so neither can
    /// drift from the other. `create` asks again with fewer classes only when the caller left the range
    /// unspecified.
    fn for_count(range: BlockRange, count: u32) -> Result<Self, PoolCreateError> {
        let minimum = 64usize
            .checked_shl((count as usize - 1) as u32 / 2)
            .ok_or(PoolCreateError::InvalidExtent)?;
        Self::build(range.min, count, minimum)
    }

    fn create(bound: usize, range: Option<BlockRange>) -> Result<Self, PoolCreateError> {
        let exact = range.is_some();
        let range = range.unwrap_or(BlockRange {
            min: MIN_BLOCK,
            max: DEFAULT_MAX,
        });
        // Both bounds come from `BlockRange::new` or from the default below, and each has been rounded
        // up to a power of two at or above `MIN_BLOCK` with `min <= max` required — that normalization
        // is what keeps this subtraction and the one in `claim` from underflowing.
        let mut count = range.max.ilog2() - range.min.ilog2() + 1;
        loop {
            let required = Self::for_count(range, count)?;
            if required.layout.size() <= bound {
                let mut low = required.classes[0].slots;
                let mut high = bound / (MIN_BLOCK + 1) + 1;
                while low + 1 < high {
                    let middle = low + (high - low) / 2;
                    if Self::build(range.min, count, middle)
                        .is_ok_and(|geometry| geometry.layout.size() <= bound)
                    {
                        low = middle;
                    } else {
                        high = middle;
                    }
                }
                return Self::build(range.min, count, low);
            }
            if exact || count == 1 {
                return Err(PoolCreateError::RequiredExtent {
                    required: required.layout.size(),
                    available: bound,
                });
            }
            count -= 1;
        }
    }

    /// Lays out one class per power-of-two size, and refuses a geometry it cannot describe.
    ///
    /// `min` doubles as an alignment: `bytes` is both the block size of class `index` and the alignment
    /// its payload area is extended to, which is what makes every block in that class `bytes`-aligned
    /// once the allocation base is aligned to the largest class. So `min` must be a power of two, and
    /// `Layout`'s own validation of the `align` argument was the only thing enforcing it; the check
    /// below states it where the geometry consumes it. `min` arrives from a caller's `BlockRange` here
    /// and from a peer-written `Info` through `recorded`, so a bad one is a typed refusal in both builds
    /// rather than a debug assertion.
    ///
    /// A count of zero is refused for the same reason as a count too large to shift: it builds a
    /// geometry with no class, and `info` reads the first class to describe one. The round trip
    /// `recorded` takes then has no answer rather than an unequal one, so the refusal belongs here,
    /// where every geometry is made, and not at a caller that would have to remember it.
    fn build(min: usize, count: u32, base: usize) -> Result<Self, PoolCreateError> {
        // Doubling `min` cannot survive more than one class per address bit, so a wider set is already
        // inadmissible and the loop below would reject it. Refusing it here is what keeps a
        // peer-written `Info.classes` count — which reaches this function through `recorded` — from
        // sizing the reserve below before that loop ever runs.
        if count == 0 || u64::from(count) > u64::from(usize::BITS) {
            return Err(PoolCreateError::InvalidExtent);
        }
        let mut layout = AllocLayout::new::<RcHeader<Storage>>();
        let mut classes = Vec::with_capacity(count as usize);
        for index in 0..count as usize {
            let bytes = min
                .checked_shl(index as u32)
                .ok_or(PoolCreateError::InvalidExtent)?;
            if !bytes.is_power_of_two() {
                return Err(PoolCreateError::InvalidExtent);
            }
            let slots = base >> (index / 2);
            let controls = AllocLayout::array::<AtomicU32>(slots)
                .map_err(|_| PoolCreateError::InvalidExtent)?;
            let (next, controls) = layout
                .extend(controls)
                .map_err(|_| PoolCreateError::InvalidExtent)?;
            let payload = AllocLayout::from_size_align(
                bytes
                    .checked_mul(slots)
                    .ok_or(PoolCreateError::InvalidExtent)?,
                bytes,
            )
            .map_err(|_| PoolCreateError::InvalidExtent)?;
            let (next, payload) = next
                .extend(payload)
                .map_err(|_| PoolCreateError::InvalidExtent)?;
            layout = next;
            classes.push(Class {
                bytes,
                slots,
                controls,
                payload,
            });
        }
        let layout = layout.pad_to_align();
        // The refusal belongs here rather than at the first peer's open: a class wider than the
        // alignment every mapping of a region is placed on cannot be reproduced at the far end, so
        // the geometry is inadmissible even when this process can use it.
        if layout.align() > crate::mem::REGION_ALIGN {
            return Err(PoolCreateError::ClassTooWide(layout.align()));
        }
        Ok(Self {
            layout,
            classes: classes.into_boxed_slice(),
        })
    }

    fn info(&self) -> Info {
        let first = self.classes[0];
        Info {
            min: first.bytes as u64,
            base: first.slots as u64,
            classes: self.classes.len() as u64,
        }
    }

    fn recorded(info: Info, extent: usize) -> Option<Self> {
        let geometry = Self::build(
            usize::try_from(info.min).ok()?,
            u32::try_from(info.classes).ok()?,
            usize::try_from(info.base).ok()?,
        )
        .ok()?;
        (geometry.layout.size() == extent && geometry.info() == info).then_some(geometry)
    }

    fn config(&self) -> Config {
        Config {
            info: self.info(),
            layout: self.layout,
        }
    }
}

pub(crate) struct Storage;

impl SharedSchema for Storage {
    const SCHEMA: SchemaKey = SchemaKey::new(schema_id("evering.pool"), 1);
}

unsafe impl header::Layout for Storage {
    type Config = Config;
    type Info = Info;

    const MAGIC: header::Magic = 0xB100;

    fn storage(conf: &Config) -> Option<AllocLayout> {
        Some(conf.layout)
    }

    fn info(conf: &Config, _: LayoutContext) -> Info {
        conf.info
    }

    unsafe fn init(destination: *mut Self, conf: Config) -> header::Status {
        let Some(geometry) = Geometry::recorded(conf.info, conf.layout.size()) else {
            return header::Status::Corrupted;
        };
        let base = destination
            .cast::<u8>()
            .wrapping_sub(core::mem::offset_of!(RcHeader<Self>, inner));
        unsafe { destination.write(Self) };
        for class in geometry.classes.iter() {
            for slot in 0..class.slots {
                unsafe {
                    class
                        .control(base, slot)
                        .cast_mut()
                        .write(AtomicU32::new(State::free(0).0));
                }
            }
        }
        header::Status::Initialized
    }

    fn attach(&self, _: &Config) -> header::Status {
        // Nothing to re-derive: `Config`'s only constructor is `Geometry::config`, so every conf
        // that reaches here already names a geometry that built. Re-running `recorded` would ask
        // `build` to disagree with itself.
        header::Status::Initialized
    }

    fn recover(context: RecoveryContext<'_, Self>) -> bool {
        let Some(geometry) = Geometry::recorded(context.info, context.extent) else {
            return false;
        };
        for class in geometry.classes.iter() {
            for slot in 0..class.slots {
                let control = context.control(*class, slot);
                let mut raw = control.load(Ordering::Acquire);
                loop {
                    let state = State(raw);
                    if !state.canonical() {
                        return false;
                    }
                    if !state.allocated() || state.owner() != Some(context.dead) {
                        break;
                    }
                    match control.compare_exchange_weak(
                        raw,
                        State::free(state.generation()).0,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => break,
                        Err(observed) => raw = observed,
                    }
                }
            }
        }
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolId(dir::Id<Storage>);

impl PoolId {
    pub const BYTE_LEN: usize = dir::ID_BYTES;

    /// Reconstructs a pool identifier from its wire parts without validation.
    ///
    /// # Safety
    ///
    /// This is a raw naming capability: a bare port that holds a static roster must be able to name
    /// a pool by region, slab, entry, and generation. Validation of those parts (bounds, liveness)
    /// is the caller's obligation, not this constructor's — see `Port::from_parts` for the validated
    /// counterpart.
    pub const fn new(
        region: crate::place::RegionId,
        slab: u32,
        entry: u32,
        generation: usize,
    ) -> Self {
        Self(dir::Id::from_parts(region, slab, entry, generation))
    }

    pub const fn parts(self) -> (crate::place::RegionId, u32, u32, usize) {
        self.0.parts()
    }

    pub fn to_bytes(self) -> [u8; Self::BYTE_LEN] {
        self.0.to_bytes()
    }

    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        dir::Id::from_bytes(bytes).map(Self)
    }
}

pub struct Pool {
    id: PoolId,
    mapped: Mapped<RcHeader<Storage>>,
    geometry: Geometry,
}

impl Pool {
    pub const fn id(&self) -> PoolId {
        self.id
    }

    pub fn as_ref(&self) -> PoolRef<'_> {
        PoolRef {
            base: self.mapped.pointer().cast(),
            geometry: &self.geometry,
            id: self.mapped.layout_id(),
            owner: self.mapped.peer(),
            _mapped: &self.mapped,
        }
    }

    pub fn range(&self) -> BlockRange {
        let first = self.geometry.classes[0].bytes;
        let last = self.geometry.classes.last().unwrap().bytes;
        BlockRange {
            min: first,
            max: last,
        }
    }

    pub fn class(&self, index: usize) -> Option<ClassInfo> {
        self.geometry.classes.get(index).map(|class| ClassInfo {
            bytes: class.bytes,
            slots: class.slots,
        })
    }

    fn new(id: PoolId, mapped: Mapped<RcHeader<Storage>>, geometry: Geometry) -> Self {
        Self {
            id,
            mapped,
            geometry,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClassInfo {
    pub bytes: usize,
    pub slots: usize,
}

#[derive(Clone, Copy)]
pub struct PoolRef<'p> {
    base: NonNull<u8>,
    geometry: &'p Geometry,
    id: crate::place::LayoutId,
    owner: crate::mem::Peer,
    _mapped: &'p Mapped<RcHeader<Storage>>,
}

// The mapping outlives `'p`; geometry is immutable, and a slot CAS grants the
// only mutable payload authority.
unsafe impl Send for PoolRef<'_> {}
unsafe impl Sync for PoolRef<'_> {}

impl<'p> PoolRef<'p> {
    /// `slot`'s authority word in `class`, as a borrow of the mapping this handle holds.
    ///
    /// Every allocation, release and transfer read or restatement of a control word goes through
    /// here, so the one address-to-borrow step in the live path is checked in one place: the
    /// mapping outlives `'p`, `&self` is shorter than `'p`, and a word reached through a handle
    /// cannot outlive the handle that named it.
    fn control(&self, class: Class, slot: usize) -> &AtomicU32 {
        unsafe { &*class.control(self.base.as_ptr(), slot) }
    }

    pub fn reserve<T: Repr>(&self) -> Result<Vacant<'p, T>, DeclineCause> {
        let () = T::VALID;
        self.claim(AllocLayout::new::<T>())
            .map(|(allocation, pointer)| Vacant {
                allocation,
                pointer: pointer.cast(),
                _value: PhantomData,
            })
    }

    pub fn put<T: Repr>(&self, value: T) -> Result<Block<'p, T>, Decline<T>> {
        match self.reserve() {
            Ok(vacant) => Ok(vacant.write(value)),
            Err(cause) => Err(cause.decline(value)),
        }
    }

    pub fn copy<T: Repr + Copy>(&self, values: &[T]) -> Result<Block<'p, [T]>, DeclineCause> {
        self.init(values.len(), |index| values[index])
    }

    pub fn init<T: Repr>(
        &self,
        len: usize,
        mut make: impl FnMut(usize) -> T,
    ) -> Result<Block<'p, [T]>, DeclineCause> {
        let mut vacant = self.reserve_slice::<T>(len)?;
        for (index, value) in vacant.as_uninit().iter_mut().enumerate() {
            value.write(make(index));
        }
        Ok(unsafe { vacant.assume_all_init() })
    }

    pub fn reserve_bytes(&self, len: usize) -> Result<Vacant<'p, [MaybeUninit<u8>]>, DeclineCause> {
        self.reserve_slice(len)
    }

    fn reserve_slice<T: Repr>(
        &self,
        len: usize,
    ) -> Result<Vacant<'p, [MaybeUninit<T>]>, DeclineCause> {
        let () = T::VALID;
        let layout = AllocLayout::array::<T>(len).map_err(|_| DeclineCause::Overflow)?;
        self.claim(layout).map(|(allocation, pointer)| Vacant {
            allocation,
            pointer: NonNull::slice_from_raw_parts(pointer.cast(), len),
            _value: PhantomData,
        })
    }

    fn claim(&self, layout: AllocLayout) -> Result<(Allocation<'p>, NonNull<u8>), DeclineCause> {
        if layout.size() == 0 {
            return Ok((
                Allocation::empty(*self),
                NonNull::new(layout.align() as *mut u8).unwrap(),
            ));
        }
        let Some(needed) = layout
            .size()
            .max(layout.align())
            .checked_next_power_of_two()
        else {
            return Err(DeclineCause::TooWide);
        };
        let first = self.geometry.classes[0].bytes;
        // The class index is a difference of logarithms, which is exact only because `build` requires
        // every class size to be a power of two; the same requirement is what makes a class's blocks
        // `bytes`-aligned.
        let preferred = needed.max(first).ilog2() - first.ilog2();
        if preferred as usize >= self.geometry.classes.len() {
            return Err(DeclineCause::TooWide);
        }
        let mut corrupted = false;
        for (index, class) in self
            .geometry
            .classes
            .iter()
            .enumerate()
            .skip(preferred as usize)
        {
            let start = index.wrapping_mul(0x9e37) % class.slots;
            for probe in 0..class.slots {
                let slot = (start + probe) % class.slots;
                let control = self.control(*class, slot);
                let free = State(control.load(Ordering::Acquire));
                if !free.canonical() {
                    // Owner bits with the allocation bit clear: `State` has no constructor that
                    // produces that word, so the slot is damaged rather than busy. Keep scanning,
                    // because one damaged word must not retire a free list that still has room, and
                    // remember it, because a capacity answer here sends the caller waiting for a
                    // release that cannot change the word. Checked before the arithmetic below so a
                    // damaged generation field cannot hide the damage.
                    corrupted = true;
                    continue;
                }
                let generation = free.generation().saturating_add(1);
                let Some(local) = State::local(generation, self.owner.slot()) else {
                    continue;
                };
                if !free.allocated()
                    && free.generation() < MAX_GENERATION
                    && control
                        .compare_exchange(free.0, local.0, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                {
                    let pointer = class.payload(self.base.as_ptr(), slot);
                    return Ok((
                        Allocation {
                            pool: *self,
                            class: index,
                            slot,
                            generation,
                            live: true,
                        },
                        unsafe { NonNull::new_unchecked(pointer) },
                    ));
                }
            }
        }
        if corrupted {
            // A damaged word is not a shortage, and the difference is the whole point: a caller that
            // hears `NoRoom` retries forever, and the region stays damaged either way.
            return Err(DeclineCause::Corrupted);
        }
        Err(DeclineCause::NoRoom {
            waits: RoomWait::Release,
        })
    }

    /// Writes an authority word naming a state no participant writes into one slot of `class`.
    /// Test-only: the control array's address is this module's layout knowledge, so a scribbled word
    /// is otherwise unreachable from a test — a peer's stray write and a crash that lands between
    /// the read and the CAS here both leave exactly this shape.
    #[cfg(test)]
    pub(crate) fn corrupt_authority_word_for_test(&self, class: usize, slot: usize) {
        let class = self.geometry.classes[class];
        let control = self.control(class, slot);
        control.store(State(1).0, Ordering::Relaxed);
    }

    fn release(self, class: usize, slot: usize, generation: u32) {
        let class = self.geometry.classes[class];
        let control = self.control(class, slot);
        let local = State::local(generation, self.owner.slot()).unwrap();
        let released = control.compare_exchange(
            local.0,
            State::free(generation).0,
            Ordering::Release,
            Ordering::Relaxed,
        );
        debug_assert_eq!(released, Ok(local.0));
    }

    pub(crate) fn adopt_id<H: Repr, T: Repr + Shape + ?Sized>(
        self,
        transfer: &Token<H>,
        expected: crate::msg::TypeId,
    ) -> Result<Block<'p, T>, AdoptError> {
        let token = &transfer.token;
        if token.id != expected {
            return Err(AdoptError::Type);
        }
        let metadata = token.metadata().ok_or(AdoptError::Type)?;
        let layout = T::layout(metadata).map_err(|_| AdoptError::Type)?;
        let (allocation, raw) = if layout.size() == 0 {
            (self.takeover(transfer)?, layout.align() as *mut u8)
        } else {
            let class = *self
                .geometry
                .classes
                .get(token.at.class)
                .ok_or(AdoptError::NoSlot)?;
            if token.generation == 0
                || token.at.slot >= class.slots
                || layout.size() > class.bytes
                || layout.align() > class.bytes
            {
                return Err(AdoptError::NoSlot);
            }
            (
                self.takeover(transfer)?,
                class.payload(self.base.as_ptr(), token.at.slot),
            )
        };
        let pointer = unsafe { metadata.as_ptr::<T>(raw) };
        Ok(Block {
            _allocation: allocation,
            pointer: NonNull::new(pointer).ok_or(AdoptError::NoSlot)?,
        })
    }

    pub(crate) fn takeover<H: Repr>(
        self,
        transfer: &Token<H>,
    ) -> Result<Allocation<'p>, AdoptError> {
        let token = &transfer.token;
        if token.generation == 0 {
            if token.pool != self.id {
                return Err(AdoptError::Pool);
            }
            return Ok(Allocation::empty(self));
        }
        if token.generation > MAX_GENERATION {
            return Err(AdoptError::Owned);
        }
        if token.pool != self.id {
            return Err(AdoptError::Pool);
        }
        let class = *self
            .geometry
            .classes
            .get(token.at.class)
            .ok_or(AdoptError::NoSlot)?;
        if token.at.slot >= class.slots {
            return Err(AdoptError::NoSlot);
        }
        let control = self.control(class, token.at.slot);
        let detached = State::detached(token.generation);
        let local = State::local(token.generation, self.owner.slot()).unwrap();
        control
            .compare_exchange(detached.0, local.0, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| AdoptError::Owned)?;
        Ok(Allocation {
            pool: self,
            class: token.at.class,
            slot: token.at.slot,
            generation: token.generation,
            live: true,
        })
    }
}

pub(crate) struct Allocation<'p> {
    pool: PoolRef<'p>,
    class: usize,
    slot: usize,
    generation: u32,
    live: bool,
}

impl<'p> Allocation<'p> {
    fn empty(pool: PoolRef<'p>) -> Self {
        Self {
            pool,
            class: 0,
            slot: 0,
            generation: 0,
            live: false,
        }
    }

    pub(crate) fn detach(&mut self) -> Result<(), AdoptError> {
        if !self.live {
            return Ok(());
        }
        let class = self.pool.geometry.classes[self.class];
        let control = self.pool.control(class, self.slot);
        let local = State::local(self.generation, self.pool.owner.slot()).unwrap();
        control
            .compare_exchange(
                local.0,
                State::detached(self.generation).0,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| AdoptError::Owned)?;
        self.live = false;
        Ok(())
    }

    /// Gives up a block whose control word another participant restated.
    ///
    /// A failed `detach` proves the word is no longer ours to change, so the
    /// release a `Drop` would attempt would act on a block someone else now owns.
    pub(crate) fn abandon(&mut self) {
        self.live = false;
    }
}

impl Drop for Allocation<'_> {
    fn drop(&mut self) {
        if self.live {
            self.pool.release(self.class, self.slot, self.generation);
        }
    }
}

/// A process-local linear transfer. The allocation remains locally owned until
/// the matching queue publication detaches it.
pub struct Transfer<'p, H: Repr> {
    allocation: Allocation<'p>,
    pub(crate) token: Token<H>,
}

impl<H: Repr + core::fmt::Debug> core::fmt::Debug for Transfer<'_, H> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Transfer")
            .field("header", &self.token.header)
            .finish_non_exhaustive()
    }
}

impl<'p, H: Repr> Transfer<'p, H> {
    pub fn map<T: Repr>(self, map: impl FnOnce(H) -> T) -> Transfer<'p, T> {
        Transfer {
            allocation: self.allocation,
            token: self.token.map(map),
        }
    }

    pub fn update(&mut self, update: impl FnOnce(&mut H)) {
        self.token.update(update);
    }

    pub(crate) fn into_parts(self) -> (Allocation<'p>, Token<H>) {
        (self.allocation, self.token)
    }

    pub(crate) fn from_parts(allocation: Allocation<'p>, token: Token<H>) -> Self {
        Self { allocation, token }
    }
}

pub struct Vacant<'p, T: ?Sized> {
    allocation: Allocation<'p>,
    pointer: NonNull<T>,
    _value: PhantomData<T>,
}

// Allocation is unique and remains bound to the mapped Pool lifetime.
unsafe impl<T: ?Sized + Send> Send for Vacant<'_, T> {}

impl<T: ?Sized> core::fmt::Debug for Vacant<'_, T> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.debug_struct("Vacant").finish_non_exhaustive()
    }
}

impl<'p, T> Vacant<'p, T> {
    pub fn write(self, value: T) -> Block<'p, T> {
        let this = ManuallyDrop::new(self);
        unsafe { this.pointer.as_ptr().write(value) };
        Block {
            _allocation: unsafe { ptr::read(&this.allocation) },
            pointer: this.pointer,
        }
    }
}

impl<'p, T> Vacant<'p, [MaybeUninit<T>]> {
    pub fn as_uninit(&mut self) -> &mut [MaybeUninit<T>] {
        unsafe { self.pointer.as_mut() }
    }

    /// # Safety
    /// Every one of the `len` elements behind the pointer must be initialized: the returned
    /// `Block<[T]>` dereferences all of them. The block's size and alignment are `claim`'s business,
    /// not this function's.
    unsafe fn assume_all_init(self) -> Block<'p, [T]> {
        // `PoolRef::init` is the only caller, and its loop writes each element before reaching here.
        let len = self.pointer.len();
        let this = ManuallyDrop::new(self);
        Block {
            _allocation: unsafe { ptr::read(&this.allocation) },
            pointer: NonNull::slice_from_raw_parts(this.pointer.cast(), len),
        }
    }
}

impl<'p> Vacant<'p, [MaybeUninit<u8>]> {
    /// # Safety
    /// The first `written` bytes must be initialized.
    pub unsafe fn assume_init(self, written: usize) -> Result<Block<'p, [u8]>, Self> {
        if written > self.pointer.len() {
            return Err(self);
        }
        let this = ManuallyDrop::new(self);
        Ok(Block {
            _allocation: unsafe { ptr::read(&this.allocation) },
            pointer: NonNull::slice_from_raw_parts(this.pointer.cast(), written),
        })
    }
}

pub struct Block<'p, T: ?Sized> {
    _allocation: Allocation<'p>,
    pointer: NonNull<T>,
}

unsafe impl<T: ?Sized + Send> Send for Block<'_, T> {}
unsafe impl<T: ?Sized + Sync> Sync for Block<'_, T> {}

impl<T: ?Sized> Deref for Block<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { self.pointer.as_ref() }
    }
}

impl<T: ?Sized> core::ops::DerefMut for Block<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { self.pointer.as_mut() }
    }
}

impl<'p, T: Repr + Shape + ?Sized> Block<'p, T> {
    fn transfer_with<H: Repr>(self, header: H, id: crate::msg::TypeId) -> Transfer<'p, H> {
        let this = ManuallyDrop::new(self);
        let metadata = T::metadata(this.pointer.as_ptr());
        let allocation = unsafe { ptr::read(&this._allocation) };
        let generation = allocation.generation;
        let token = Token {
            token: crate::token::PoolToken::new(
                allocation.pool.id,
                ClassSlot {
                    class: allocation.class,
                    slot: allocation.slot,
                },
                generation,
                metadata,
                id,
            ),
            header,
        };
        Transfer { allocation, token }
    }

    pub fn transfer<H: Repr>(self, header: H) -> Transfer<'p, H> {
        self.transfer_with(header, crate::msg::type_id::<H, T>())
    }

    pub fn encode(self, schema: SchemaKey) -> Transfer<'p, crate::Encoded> {
        self.transfer_with(crate::Encoded(()), crate::Encoded::id::<T>(schema))
    }
}

pub(crate) fn create(
    directory: &dir::MapDirectory,
    heap: &MapTalc,
    bound: usize,
    range: Option<BlockRange>,
) -> Result<Pool, PoolCreateError> {
    let geometry = Geometry::create(bound, range)?;
    let config = geometry.config();
    let region = heap.region();
    directory
        .create_in::<Storage>(heap, config)
        .map(|(id, mapped)| Pool::new(PoolId(id), mapped, geometry))
        .map_err(|error| match error.declined(region) {
            Some(cause) => PoolCreateError::Declined(cause),
            None => PoolCreateError::Record,
        })
}

pub(crate) fn open(
    directory: &dir::MapDirectory,
    id: PoolId,
) -> Result<Pool, crate::OpenPoolError> {
    let (mapped, geometry) = directory
        .open_recorded(id.0, |info, extent| {
            Geometry::recorded(info, extent).map(|geometry| (geometry.config(), geometry))
        })
        .map_err(|cause| crate::OpenPoolError::of(id, cause))?;
    Ok(Pool::new(id, mapped, geometry))
}

pub(crate) fn release_token<H: Repr>(
    directory: &dir::MapDirectory,
    transfer: &Token<H>,
    owner: Option<u8>,
) -> bool {
    let token = &transfer.token;
    if token.generation == 0 {
        return directory
            .inspect_layout::<Storage, _>(token.pool, |_, _| ())
            .is_some();
    }
    if token.generation > MAX_GENERATION || token.metadata().is_none() {
        return false;
    }
    directory
        .inspect_layout::<Storage, _>(token.pool, |header, extent| {
            let Some(geometry) = Geometry::recorded(header.layout_info(), extent) else {
                return false;
            };
            let Some(class) = geometry.classes.get(token.at.class) else {
                return false;
            };
            if token.at.slot >= class.slots {
                return false;
            }
            let control = header.control(*class, token.at.slot);
            let mut raw = control.load(Ordering::Acquire);
            loop {
                let state = State(raw);
                if !state.canonical() || state.generation() < token.generation {
                    return false;
                }
                if state.generation() > token.generation || !state.allocated() {
                    return true;
                }
                if state.owner().is_some_and(|actual| Some(actual) != owner) {
                    return false;
                }
                match control.compare_exchange_weak(
                    raw,
                    State::free(token.generation).0,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => return true,
                    Err(observed) => raw = observed,
                }
            }
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod geometry_tests {
    use super::{Geometry, Info, PoolCreateError, RcHeader, Storage};

    /// A peer writes the `Info` that `recorded` rebuilds a geometry from, so the class count is peer
    /// input before it is arithmetic input. The loop in `build` rejects a shift that cannot be
    /// represented, but it runs after the class vector has been sized from that same number, which is
    /// why the refusal has to come first.
    #[test]
    fn an_oversized_class_count_is_refused_before_it_is_reserved() {
        assert_eq!(
            Geometry::build(64, u32::MAX, 64).err(),
            Some(PoolCreateError::InvalidExtent)
        );
        assert_eq!(
            Geometry::build(64, usize::BITS + 1, 64).err(),
            Some(PoolCreateError::InvalidExtent)
        );
        // The bound itself stays admissible: nothing is rejected that the loop would have accepted.
        assert!(Geometry::build(64, usize::BITS, 64).is_err());
        assert!(Geometry::build(64, 3, 64).is_ok());
    }

    /// The other end of the same door. A count that is not too large is not thereby usable: `info`
    /// projects a geometry by reading its first class, so an empty class list has no `Info` and the
    /// round trip `recorded` makes cannot be taken. The number arrives from a peer's record, so this
    /// is a refusal `build` owes and a caller cannot supply on its behalf.
    #[test]
    fn an_empty_class_count_is_refused_rather_than_projected() {
        let empty = Info {
            min: 64,
            base: 64,
            classes: 0,
        };
        assert!(Geometry::recorded(empty, core::mem::size_of::<RcHeader<Storage>>()).is_none());
        assert_eq!(
            Geometry::build(64, 0, 64).err(),
            Some(PoolCreateError::InvalidExtent)
        );
    }
}

#[cfg(test)]
mod state_tests {
    use super::{ALLOCATED, GENERATION_SHIFT, MAX_GENERATION, State};

    #[test]
    fn authority_word_rejects_owner_without_allocation_and_generation_wrap() {
        assert!(!State(1).canonical());
        assert!(State::free(MAX_GENERATION).canonical());
        assert!(State::local(MAX_GENERATION, 0).is_some());
        assert!(State::local(MAX_GENERATION + 1, 0).is_none());
        assert_eq!(State::detached(7).0, (7 << GENERATION_SHIFT) | ALLOCATED);
    }
}
