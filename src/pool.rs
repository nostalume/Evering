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
    mem::Mapped,
    msg::Repr,
    schema::{LayoutContext, LayoutInfo, SchemaKey, SharedSchema, schema_id},
    talc::MapTalc,
    token::{Shape, Span, Token},
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RangeError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolCreateError {
    InvalidExtent,
    RequiredExtent { required: usize, available: usize },
    Storage,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReserveError<T> {
    BlockTooLarge(T),
    Unavailable(T),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdoptError {
    Pool,
    Span,
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
    fn control(self, base: *const u8, slot: usize) -> *const AtomicU32 {
        base.wrapping_add(self.controls)
            .cast::<AtomicU32>()
            .wrapping_add(slot)
    }

    fn payload(self, base: *mut u8, slot: usize) -> *mut u8 {
        base.wrapping_add(self.payload + slot * self.bytes)
    }
}

struct Geometry {
    layout: AllocLayout,
    classes: Box<[Class]>,
}

impl Geometry {
    fn create(bound: usize, range: Option<BlockRange>) -> Result<Self, PoolCreateError> {
        let exact = range.is_some();
        let range = range.unwrap_or(BlockRange {
            min: MIN_BLOCK,
            max: DEFAULT_MAX,
        });
        let mut count = range.max.ilog2() - range.min.ilog2() + 1;
        loop {
            let minimum = 64usize
                .checked_shl((count as usize - 1) as u32 / 2)
                .ok_or(PoolCreateError::InvalidExtent)?;
            let required = Self::build(range.min, count, minimum)?;
            if required.layout.size() <= bound {
                let mut low = minimum;
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

    fn build(min: usize, count: u32, base: usize) -> Result<Self, PoolCreateError> {
        let mut layout = AllocLayout::new::<RcHeader<Storage>>();
        let mut classes = Vec::with_capacity(count as usize);
        for index in 0..count as usize {
            let bytes = min
                .checked_shl(index as u32)
                .ok_or(PoolCreateError::InvalidExtent)?;
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
        Ok(Self {
            layout: layout.pad_to_align(),
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

    fn attach(&self, conf: &Config) -> header::Status {
        if Geometry::recorded(conf.info, conf.layout.size()).is_some() {
            header::Status::Initialized
        } else {
            header::Status::Corrupted
        }
    }

    fn recover(context: RecoveryContext<'_, Self>) -> bool {
        let Some(geometry) = Geometry::recorded(context.info, context.extent) else {
            return false;
        };
        for class in geometry.classes.iter() {
            for slot in 0..class.slots {
                let control = unsafe { &*class.control(context.base, slot) };
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
    pub const fn new(
        region: crate::schema::RegionId,
        slab: u32,
        entry: u32,
        generation: usize,
    ) -> Self {
        Self(dir::Id::from_parts(region, slab, entry, generation))
    }

    pub const fn parts(self) -> (crate::schema::RegionId, u32, u32, usize) {
        self.0.parts()
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
    id: crate::schema::LayoutId,
    owner: crate::mem::Peer,
    _mapped: &'p Mapped<RcHeader<Storage>>,
}

// The mapping outlives `'p`; geometry is immutable, and a slot CAS grants the
// only mutable payload authority.
unsafe impl Send for PoolRef<'_> {}
unsafe impl Sync for PoolRef<'_> {}

impl<'p> PoolRef<'p> {
    pub fn reserve<T: Repr>(&self) -> Result<Vacant<'p, T>, ReserveError<()>> {
        let () = T::VALID;
        self.claim(AllocLayout::new::<T>())
            .map(|(allocation, pointer)| Vacant {
                allocation,
                pointer: pointer.cast(),
                _value: PhantomData,
            })
    }

    pub fn put<T: Repr>(&self, value: T) -> Result<Block<'p, T>, ReserveError<T>> {
        match self.reserve() {
            Ok(vacant) => Ok(vacant.write(value)),
            Err(ReserveError::BlockTooLarge(())) => Err(ReserveError::BlockTooLarge(value)),
            Err(ReserveError::Unavailable(())) => Err(ReserveError::Unavailable(value)),
        }
    }

    pub fn copy<T: Repr + Copy>(&self, values: &[T]) -> Result<Block<'p, [T]>, ReserveError<()>> {
        self.init(values.len(), |index| values[index])
    }

    pub fn init<T: Repr>(
        &self,
        len: usize,
        mut make: impl FnMut(usize) -> T,
    ) -> Result<Block<'p, [T]>, ReserveError<()>> {
        let mut vacant = self.reserve_slice::<T>(len)?;
        for (index, value) in vacant.as_uninit().iter_mut().enumerate() {
            value.write(make(index));
        }
        Ok(unsafe { vacant.assume_all_init() })
    }

    pub fn reserve_bytes(
        &self,
        len: usize,
    ) -> Result<Vacant<'p, [MaybeUninit<u8>]>, ReserveError<()>> {
        self.reserve_slice(len)
    }

    fn reserve_slice<T: Repr>(
        &self,
        len: usize,
    ) -> Result<Vacant<'p, [MaybeUninit<T>]>, ReserveError<()>> {
        let () = T::VALID;
        let layout = AllocLayout::array::<T>(len).map_err(|_| ReserveError::BlockTooLarge(()))?;
        self.claim(layout).map(|(allocation, pointer)| Vacant {
            allocation,
            pointer: NonNull::slice_from_raw_parts(pointer.cast(), len),
            _value: PhantomData,
        })
    }

    fn claim(
        &self,
        layout: AllocLayout,
    ) -> Result<(Allocation<'p>, NonNull<u8>), ReserveError<()>> {
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
            return Err(ReserveError::BlockTooLarge(()));
        };
        let first = self.geometry.classes[0].bytes;
        let preferred = needed.max(first).ilog2() - first.ilog2();
        if preferred as usize >= self.geometry.classes.len() {
            return Err(ReserveError::BlockTooLarge(()));
        }
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
                let control = unsafe { &*class.control(self.base.as_ptr(), slot) };
                let free = State(control.load(Ordering::Acquire));
                let generation = free.generation().saturating_add(1);
                let Some(local) = State::local(generation, self.owner.slot()) else {
                    continue;
                };
                if free.canonical()
                    && !free.allocated()
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
        Err(ReserveError::Unavailable(()))
    }

    fn release(self, class: usize, slot: usize, generation: u32) {
        let class = self.geometry.classes[class];
        let control = unsafe { &*class.control(self.base.as_ptr(), slot) };
        let local = State::local(generation, self.owner.slot()).unwrap();
        let released = control.compare_exchange(
            local.0,
            State::free(generation).0,
            Ordering::Release,
            Ordering::Relaxed,
        );
        debug_assert_eq!(released, Ok(local.0));
    }

    pub(crate) fn adopt<H: Repr, T: Repr + Shape + ?Sized>(
        self,
        transfer: &Token<H>,
    ) -> Result<Block<'p, T>, AdoptError> {
        let token = &transfer.token;
        if token.id != crate::msg::type_id::<H, T>() {
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
                .get(token.span.class)
                .ok_or(AdoptError::Span)?;
            if token.generation == 0
                || token.span.slot >= class.slots
                || layout.size() > class.bytes
                || layout.align() > class.bytes
            {
                return Err(AdoptError::Span);
            }
            (
                self.takeover(transfer)?,
                class.payload(self.base.as_ptr(), token.span.slot),
            )
        };
        let pointer = unsafe { metadata.as_ptr::<T>(raw) };
        Ok(Block {
            _allocation: allocation,
            pointer: NonNull::new(pointer).ok_or(AdoptError::Span)?,
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
            .get(token.span.class)
            .ok_or(AdoptError::Span)?;
        if token.span.slot >= class.slots {
            return Err(AdoptError::Span);
        }
        let control = unsafe { &*class.control(self.base.as_ptr(), token.span.slot) };
        let detached = State::detached(token.generation);
        let local = State::local(token.generation, self.owner.slot()).unwrap();
        control
            .compare_exchange(detached.0, local.0, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| AdoptError::Owned)?;
        Ok(Allocation {
            pool: self,
            class: token.span.class,
            slot: token.span.slot,
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
        let control = unsafe { &*class.control(self.pool.base.as_ptr(), self.slot) };
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

    unsafe fn assume_all_init(self) -> Block<'p, [T]> {
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
    pub fn transfer<H: Repr>(self, header: H) -> Transfer<'p, H> {
        let this = ManuallyDrop::new(self);
        let metadata = T::metadata(this.pointer.as_ptr());
        let allocation = unsafe { ptr::read(&this._allocation) };
        let generation = allocation.generation;
        let token = Token {
            token: crate::token::PoolToken::new(
                allocation.pool.id,
                Span {
                    class: allocation.class,
                    slot: allocation.slot,
                },
                generation,
                metadata,
                crate::msg::type_id::<H, T>(),
            ),
            header,
        };
        Transfer { allocation, token }
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
    directory
        .create_in::<Storage>(heap, config, config.layout)
        .map(|(id, mapped)| Pool::new(PoolId(id), mapped, geometry))
        .map_err(|_| PoolCreateError::Storage)
}

pub(crate) fn open(directory: &dir::MapDirectory, id: PoolId) -> Option<Pool> {
    let (mapped, geometry) = directory.open_recorded(id.0, |info, extent| {
        Geometry::recorded(info, extent).map(|geometry| (geometry.config(), geometry))
    })?;
    Some(Pool::new(id, mapped, geometry))
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
            let Some(class) = geometry.classes.get(token.span.class) else {
                return false;
            };
            if token.span.slot >= class.slots {
                return false;
            }
            let control =
                unsafe { &*class.control(core::ptr::from_ref(header).cast(), token.span.slot) };
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
