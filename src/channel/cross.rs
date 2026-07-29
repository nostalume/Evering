use core::alloc::Layout as AllocLayout;
use core::marker::PhantomData;
use core::mem::offset_of;
use core::ptr::NonNull;
use core::sync::atomic::AtomicU8;

use alloc::sync::Arc;

use crate::channel::{ClaimError, Header, Queue, QueueOps, QueueRx, QueueTx, Repair, Slot};
use crate::header::{self, RcHeader};
use crate::mem::{Mapped, Meta};
use crate::msg::Repr;
use crate::schema::{
    LayoutContext, LayoutInfo, SchemaKey, SharedSchema, compose_schema, schema_id,
};
use crate::token::PackToken;

type Item<H, M> = PackToken<H, M>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    const fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Info {
    capacity: u64,
}

impl SharedSchema for Info {
    const SCHEMA: SchemaKey = SchemaKey::new(schema_id("evering.duplex.info"), 1);
}

unsafe impl LayoutInfo for Info {}

impl Info {
    pub(crate) fn capacity(self) -> Option<usize> {
        usize::try_from(self.capacity).ok()
    }
}

#[derive(Clone, Copy)]
pub struct Config {
    capacity: usize,
}

impl Config {
    pub(crate) const fn new(capacity: usize) -> Self {
        Self { capacity }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GeometryError {
    ZeroCapacity,
    Overflow,
}

#[derive(Clone, Copy)]
pub(crate) struct Geometry {
    layout: AllocLayout,
    slots: usize,
    capacity: usize,
    one_lap: usize,
}

impl Geometry {
    pub(crate) const fn layout(self) -> AllocLayout {
        self.layout
    }
}

#[repr(C)]
pub struct Duplex<H: Repr, M: Meta> {
    lifecycle: AtomicU8,
    left: Header,
    right: Header,
    _item: PhantomData<fn() -> Item<H, M>>,
}

impl<H: Repr, M: Meta> Duplex<H, M> {
    pub(crate) fn geometry(capacity: usize) -> Result<Geometry, GeometryError> {
        if capacity == 0 {
            return Err(GeometryError::ZeroCapacity);
        }
        let count = capacity.checked_mul(2).ok_or(GeometryError::Overflow)?;
        let slots =
            AllocLayout::array::<Slot<Item<H, M>>>(count).map_err(|_| GeometryError::Overflow)?;
        let (layout, slots) = AllocLayout::new::<RcHeader<Self>>()
            .extend(slots)
            .map_err(|_| GeometryError::Overflow)?;
        let one_lap = capacity
            .checked_add(1)
            .and_then(usize::checked_next_power_of_two)
            .ok_or(GeometryError::Overflow)?;
        Ok(Geometry {
            layout: layout.pad_to_align(),
            slots,
            capacity,
            one_lap,
        })
    }

    unsafe fn initialize_slots(destination: *mut Self, geometry: Geometry) {
        let base = destination
            .cast::<u8>()
            .wrapping_sub(offset_of!(RcHeader<Self>, inner));
        let slots = base.wrapping_add(geometry.slots).cast::<Slot<Item<H, M>>>();
        for direction in 0..2 {
            for index in 0..geometry.capacity {
                unsafe {
                    slots
                        .add(direction * geometry.capacity + index)
                        .write(Slot::new(index));
                }
            }
        }
    }
}

impl<H: Repr, M: Meta> SharedSchema for Duplex<H, M> {
    const SCHEMA: SchemaKey = compose_schema(
        compose_schema(SchemaKey::new(schema_id("evering.duplex"), 3), H::SCHEMA),
        crate::token::Token::<M>::SCHEMA,
    );
}

unsafe impl<H: Repr, M: Meta> header::Layout for Duplex<H, M> {
    type Config = Config;
    type Info = Info;

    const MAGIC: header::Magic = 0xD0A1;

    fn info(conf: &Config, _: LayoutContext) -> Info {
        match Self::geometry(conf.capacity) {
            Ok(_) => Info {
                capacity: conf.capacity as u64,
            },
            Err(_) => Info { capacity: 0 },
        }
    }

    unsafe fn init(destination: *mut Self, conf: Config) -> header::Status {
        let () = <Item<H, M> as Repr>::VALID;
        let Ok(geometry) = Self::geometry(conf.capacity) else {
            return header::Status::Corrupted;
        };
        unsafe {
            destination.write(Self {
                lifecycle: AtomicU8::new(0),
                left: Header::new(),
                right: Header::new(),
                _item: PhantomData,
            });
            Self::initialize_slots(destination, geometry);
        }
        header::Status::Initialized
    }

    fn attach(&self, conf: &Config) -> header::Status {
        if Self::geometry(conf.capacity).is_ok() {
            header::Status::Initialized
        } else {
            header::Status::Corrupted
        }
    }
}

pub struct View<H: Repr, M: Meta> {
    mapped: Arc<Mapped<RcHeader<Duplex<H, M>>>>,
    slots: NonNull<Slot<Item<H, M>>>,
    capacity: usize,
    one_lap: usize,
}

// The mapping owns the allocation. Queue slot access is exclusively admitted
// by atomically publishing slot ownership before either shared cursor moves.
unsafe impl<H: Repr, M: Meta> Send for View<H, M> {}
unsafe impl<H: Repr, M: Meta> Sync for View<H, M> {}

impl<H: Repr, M: Meta> Clone for View<H, M> {
    fn clone(&self) -> Self {
        Self {
            mapped: self.mapped.clone(),
            slots: self.slots,
            capacity: self.capacity,
            one_lap: self.one_lap,
        }
    }
}

impl<H: Repr, M: Meta> View<H, M> {
    pub(crate) fn new(mapped: Mapped<RcHeader<Duplex<H, M>>>) -> Option<Self> {
        let info = mapped.layout_info();
        let capacity = usize::try_from(info.capacity).ok()?;
        let geometry = Duplex::<H, M>::geometry(capacity).ok()?;
        let base = mapped.pointer().cast::<u8>();
        mapped.offset_of(base, geometry.layout.size()).ok()?;
        let slots = unsafe { NonNull::new_unchecked(base.as_ptr().add(geometry.slots).cast()) };
        Some(Self {
            mapped: Arc::new(mapped),
            slots,
            capacity,
            one_lap: geometry.one_lap,
        })
    }

    fn endpoint(&self, side: Side) -> Handle<H, M> {
        Handle {
            view: self.clone(),
            side,
        }
    }

    pub fn endpoints(self, side: Side) -> (QueueTx<Handle<H, M>>, QueueRx<Handle<H, M>>) {
        let tx = self.endpoint(side);
        let rx = self.endpoint(side.opposite());
        (QueueTx { tx }, QueueRx { rx })
    }

    pub fn lsplit(self) -> (QueueTx<Handle<H, M>>, QueueRx<Handle<H, M>>) {
        self.endpoints(Side::Left)
    }

    pub fn rsplit(self) -> (QueueTx<Handle<H, M>>, QueueRx<Handle<H, M>>) {
        self.endpoints(Side::Right)
    }

    pub(crate) fn layout_id(&self) -> crate::LayoutId {
        self.mapped.layout_id()
    }

    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    pub(crate) fn is_unique(&self) -> bool {
        Arc::strong_count(&self.mapped) == 1
    }

    pub(crate) fn repair(&self, dead: u8, live: u8) -> Repair {
        let mut result = Repair::None;
        for side in [Side::Left, Side::Right] {
            let handle = self.endpoint(side);
            for index in 0..self.capacity {
                match (&handle).repair(index, dead, live) {
                    Repair::None => {}
                    Repair::Recovered => result = Repair::Recovered,
                    other => return other,
                }
            }
        }
        if self.mapped.recover_member(dead) {
            result = Repair::Recovered;
        }
        result
    }

    pub(crate) fn quiesce(
        &self,
        mut discard: impl FnMut(Item<H, M>) -> Result<(), Item<H, M>>,
    ) -> Result<bool, Item<H, M>> {
        let handles = [self.endpoint(Side::Left), self.endpoint(Side::Right)];
        for handle in &handles {
            handle.close_send();
        }
        for handle in &handles {
            loop {
                match handle.claim() {
                    Ok(claim) => discard(claim.take())?,
                    Err(ClaimError::Empty) => break,
                    Err(ClaimError::Busy | ClaimError::Closed) => return Ok(false),
                }
            }
            if !handle.send_closed() || !handle.is_empty() {
                return Ok(false);
            }
        }
        for handle in &handles {
            handle.close_recv();
        }
        Ok(handles.iter().all(|handle| handle.recv_closed()))
    }

    pub(crate) fn into_mapped(self) -> Result<Mapped<RcHeader<Duplex<H, M>>>, Self> {
        match Arc::try_unwrap(self.mapped) {
            Ok(mapped) => Ok(mapped),
            Err(mapped) => Err(Self {
                mapped,
                slots: self.slots,
                capacity: self.capacity,
                one_lap: self.one_lap,
            }),
        }
    }
}

pub struct Handle<H: Repr, M: Meta> {
    view: View<H, M>,
    side: Side,
}

impl<H: Repr, M: Meta> Clone for Handle<H, M> {
    fn clone(&self) -> Self {
        Self {
            view: self.view.clone(),
            side: self.side,
        }
    }
}

impl<H: Repr, M: Meta> Queue for Handle<H, M> {
    type Item = Item<H, M>;

    fn header(&self) -> &Header {
        match self.side {
            Side::Left => &self.view.mapped.left,
            Side::Right => &self.view.mapped.right,
        }
    }

    fn buf(&self) -> &[Slot<Self::Item>] {
        let offset = usize::from(self.side == Side::Right) * self.view.capacity;
        unsafe {
            core::slice::from_raw_parts(self.view.slots.as_ptr().add(offset), self.view.capacity)
        }
    }

    fn lifecycle(&self) -> &AtomicU8 {
        &self.view.mapped.lifecycle
    }

    fn send_field(&self) -> u32 {
        match self.side {
            Side::Left => 0,
            Side::Right => 4,
        }
    }

    fn recv_field(&self) -> u32 {
        match self.side {
            Side::Left => 6,
            Side::Right => 2,
        }
    }

    fn owner(&self) -> u8 {
        self.view.mapped.peer().slot()
    }

    fn one_lap(&self) -> usize {
        self.view.one_lap
    }
}

#[cfg(test)]
mod tests {
    use super::{Duplex, GeometryError};
    use crate::talc;
    use crate::{Repr, SchemaId, SchemaKey};

    #[repr(C, align(64))]
    struct Aligned([u8; 64]);

    unsafe impl Repr for Aligned {
        const SCHEMA: SchemaKey = SchemaKey::new(SchemaId(0x414c_4947_4e45_4401), 1);
    }

    #[test]
    fn dynamic_geometry_rejects_invalid_capacity() {
        assert!(matches!(
            Duplex::<(), talc::Meta>::geometry(0),
            Err(GeometryError::ZeroCapacity)
        ));
        assert!(matches!(
            Duplex::<(), talc::Meta>::geometry(usize::MAX),
            Err(GeometryError::Overflow)
        ));
    }

    #[test]
    fn dynamic_geometry_accepts_non_power_of_two_capacity() {
        let one = Duplex::<(), talc::Meta>::geometry(1).unwrap();
        let three = Duplex::<(), talc::Meta>::geometry(3).unwrap();
        assert!(three.layout.size() > one.layout.size());
        assert_eq!(three.layout.size() % three.layout.align(), 0);
        assert_eq!(three.capacity, 3);
        assert_eq!(three.one_lap, 4);

        let aligned = Duplex::<Aligned, talc::Meta>::geometry(3).unwrap();
        assert!(aligned.layout.align() >= 64);
        assert_eq!(aligned.slots % 64, 0);
    }
}
