use core::alloc::Layout as AllocLayout;
use core::marker::PhantomData;
use core::mem::ManuallyDrop;
use core::mem::offset_of;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use alloc::sync::Arc;

use crate::header::{self, RcHeader, RecoveryContext};
use crate::mem::Mapped;
use crate::msg::Repr;
use crate::queue::{
    Claim, ClaimError, Header, Queue, Repair, ReserveError, Reserved, Slot, Staged,
    repair_slot_with,
};
use crate::schema::{
    LayoutContext, LayoutInfo, SchemaKey, SharedSchema, compose_schema, schema_id,
};
use crate::token::Token;

pub use crate::queue::{ReserveError as SendReserveError, TrySendError};

type Item<H> = Token<H>;
type ClosedMap<H> = (Mapped<RcHeader<Duplex<H>>>, usize, usize, u8);

const ROLE_SLOT_BITS: usize = usize::BITS.trailing_zeros() as usize;
const ROLE_GENERATION_SHIFT: usize = 1 + ROLE_SLOT_BITS;
const ROLE_SLOT_MASK: usize = ((1 << ROLE_SLOT_BITS) - 1) << 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
struct RoleWord(usize);

impl RoleWord {
    const MAX_GENERATION: usize = usize::MAX >> ROLE_GENERATION_SHIFT;

    const fn new(generation: usize, participant: Option<u8>, terminal: bool) -> Option<Self> {
        if generation > Self::MAX_GENERATION {
            return None;
        }
        let slot = match participant {
            Some(participant) if (participant as usize) < crate::header::PARTICIPANT_CAPACITY => {
                participant as usize + 1
            }
            Some(_) => return None,
            None => 0,
        };
        Some(Self(
            (generation << ROLE_GENERATION_SHIFT) | (slot << 1) | terminal as usize,
        ))
    }

    const fn parts(self) -> (usize, Option<u8>, bool) {
        let generation = self.0 >> ROLE_GENERATION_SHIFT;
        let slot = (self.0 & ROLE_SLOT_MASK) >> 1;
        let participant = if slot == 0 {
            None
        } else {
            Some((slot - 1) as u8)
        };
        (generation, participant, self.0 & 1 != 0)
    }

    const fn next_unowned(self, terminal: bool) -> Option<Self> {
        let (generation, _, _) = self.parts();
        let Some(generation) = generation.checked_add(1) else {
            return None;
        };
        Self::new(generation, None, terminal)
    }
}

fn recover_role(role: &AtomicUsize, dead: u8) {
    let mut raw = role.load(Ordering::Acquire);
    loop {
        let current = RoleWord(raw);
        let (generation, owner, terminal) = current.parts();
        if owner != Some(dead) {
            return;
        }
        let released = if terminal {
            RoleWord::new(generation, None, true).unwrap()
        } else {
            current
                .next_unowned(false)
                .unwrap_or_else(|| RoleWord::new(generation, None, true).unwrap())
        };
        match role.compare_exchange_weak(raw, released.0, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return,
            Err(observed) => raw = observed,
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

pub(crate) type Config = usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GeometryError {
    ZeroCapacity,
    Overflow,
}

#[derive(Clone, Copy)]
pub(crate) struct Geometry {
    pub(crate) layout: AllocLayout,
    slots: usize,
    capacity: usize,
    one_lap: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Id<H: Repr> {
    pub(crate) inner: crate::dir::Id<Duplex<H>>,
    pub(crate) capacity: usize,
}

impl<H: Repr> Copy for Id<H> {}

impl<H: Repr> Clone for Id<H> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<H: Repr> Id<H> {
    pub const fn new(
        region: crate::schema::RegionId,
        slab: u32,
        entry: u32,
        generation: usize,
        capacity: usize,
    ) -> Self {
        Self {
            inner: crate::dir::Id::from_parts(region, slab, entry, generation),
            capacity,
        }
    }

    pub const fn region(self) -> crate::schema::RegionId {
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

pub struct Port<H: Repr> {
    pub(crate) id: Id<H>,
    pub(crate) role: usize,
    pub(crate) generation: usize,
}

impl<H: Repr> Port<H> {
    pub(crate) const fn new(id: Id<H>, role: usize, generation: usize) -> Self {
        Self {
            id,
            role,
            generation,
        }
    }

    pub const fn region(&self) -> crate::schema::RegionId {
        self.id.region()
    }

    pub const fn id(&self) -> Id<H> {
        self.id
    }

    pub const fn role(&self) -> u8 {
        self.role as u8
    }

    pub const fn generation(&self) -> usize {
        self.generation
    }

    pub const fn from_parts(id: Id<H>, role: u8, generation: usize) -> Option<Self> {
        if role >= 2 || generation == 0 || generation > RoleWord::MAX_GENERATION {
            return None;
        }
        Some(Self {
            id,
            role: role as usize,
            generation,
        })
    }
}

impl<H: Repr> core::fmt::Debug for Port<H> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Port")
            .field("region", &self.region())
            .field("role", &self.role)
            .field("generation", &self.generation)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InviteError {
    Occupied,
    Terminal,
    Busy,
    Retired,
}

pub(crate) enum CloseError {
    Busy,
    Evidence,
}

#[repr(C)]
pub struct Duplex<H: Repr> {
    lifecycle: AtomicU8,
    roles: [AtomicUsize; 2],
    left: Header,
    right: Header,
    _item: PhantomData<fn() -> Item<H>>,
}

impl<H: Repr> Duplex<H> {
    fn blank() -> Self {
        Self {
            lifecycle: AtomicU8::new(0),
            roles: [const { AtomicUsize::new(0) }; 2],
            left: Header::new(),
            right: Header::new(),
            _item: PhantomData,
        }
    }

    pub(crate) fn geometry(capacity: usize) -> Result<Geometry, GeometryError> {
        if capacity == 0 {
            return Err(GeometryError::ZeroCapacity);
        }
        let count = capacity.checked_mul(2).ok_or(GeometryError::Overflow)?;
        let slots =
            AllocLayout::array::<Slot<Item<H>>>(count).map_err(|_| GeometryError::Overflow)?;
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
}

impl<H: Repr> SharedSchema for Duplex<H> {
    const SCHEMA: SchemaKey = compose_schema(
        SchemaKey::new(schema_id("evering.duplex"), 5),
        <Token<H> as SharedSchema>::SCHEMA,
    );
}

unsafe impl<H: Repr> header::Layout for Duplex<H> {
    type Config = Config;
    type Info = Info;

    const MAGIC: header::Magic = 0xD0A1;

    fn storage(conf: &Config) -> Option<AllocLayout> {
        Self::geometry(*conf).ok().map(|geometry| geometry.layout)
    }

    fn info(conf: &Config, _: LayoutContext) -> Info {
        match Self::geometry(*conf) {
            Ok(_) => Info {
                capacity: *conf as u64,
            },
            Err(_) => Info { capacity: 0 },
        }
    }

    unsafe fn init(destination: *mut Self, conf: Config) -> header::Status {
        let () = <Item<H> as Repr>::VALID;
        let Ok(geometry) = Self::geometry(conf) else {
            return header::Status::Corrupted;
        };
        let base = destination
            .cast::<u8>()
            .wrapping_sub(offset_of!(RcHeader<Self>, inner));
        let slots = base.wrapping_add(geometry.slots).cast::<Slot<Item<H>>>();
        unsafe {
            destination.write(Self::blank());
            for direction in 0..2 {
                for index in 0..geometry.capacity {
                    slots
                        .add(direction * geometry.capacity + index)
                        .write(Slot::new(index));
                }
            }
        }
        header::Status::Initialized
    }

    fn attach(&self, conf: &Config) -> header::Status {
        if Self::geometry(*conf).is_ok() {
            header::Status::Initialized
        } else {
            header::Status::Corrupted
        }
    }

    fn recover(context: RecoveryContext<'_, Self>) -> bool {
        let Ok(capacity) = usize::try_from(context.info.capacity) else {
            return false;
        };
        let Ok(geometry) = Self::geometry(capacity) else {
            return false;
        };
        if geometry.layout.size() != context.extent {
            return false;
        }
        let slots = unsafe {
            core::slice::from_raw_parts(
                context.base.add(geometry.slots).cast::<Slot<Item<H>>>(),
                capacity * 2,
            )
        };
        for (header, start) in [(&context.layout.left, 0), (&context.layout.right, capacity)] {
            let slots = &slots[start..start + capacity];
            for index in 0..capacity {
                if matches!(
                    repair_slot_with(
                        header,
                        slots,
                        geometry.one_lap,
                        index,
                        context.dead,
                        context.live,
                        |token, owner| {
                            crate::pool::release_token(context.directory, token, Some(owner))
                        },
                    ),
                    Repair::Busy(_) | Repair::Corrupted
                ) {
                    return false;
                }
            }
        }
        for role in &context.layout.roles {
            recover_role(role, context.dead);
        }
        true
    }
}

struct Role<H: Repr> {
    id: Id<H>,
    mapped: Mapped<RcHeader<Duplex<H>>>,
    routes: [Route<H>; 2],
    capacity: usize,
    one_lap: usize,
    index: usize,
    generation: usize,
    owner: u8,
}

struct Route<H: Repr> {
    header: NonNull<Header>,
    slots: NonNull<Slot<Item<H>>>,
    send_field: u32,
    recv_field: u32,
}

// Mapped owns the allocation, while Queue atomics admit every slot access.
unsafe impl<H: Repr> Send for Role<H> {}
unsafe impl<H: Repr> Sync for Role<H> {}

impl<H: Repr> Role<H> {
    fn route(&self, direction: usize) -> &Route<H> {
        &self.routes[direction]
    }
}

fn routes<H: Repr>(
    mapped: &Mapped<RcHeader<Duplex<H>>>,
    slots: NonNull<Slot<Item<H>>>,
    capacity: usize,
) -> [Route<H>; 2] {
    [
        Route {
            header: NonNull::from(&mapped.left),
            slots,
            send_field: 0,
            recv_field: 6,
        },
        Route {
            header: NonNull::from(&mapped.right),
            slots: unsafe { NonNull::new_unchecked(slots.as_ptr().add(capacity)) },
            send_field: 4,
            recv_field: 2,
        },
    ]
}

impl<H: Repr> Drop for Role<H> {
    fn drop(&mut self) {
        let authority = &self.mapped.roles[self.index];
        let mut raw = authority.load(Ordering::Acquire);
        loop {
            let current = RoleWord(raw);
            let (generation, owner, terminal) = current.parts();
            if generation != self.generation || owner != Some(self.owner) {
                return;
            }
            let released = if terminal {
                RoleWord::new(generation, None, true).unwrap()
            } else {
                current
                    .next_unowned(false)
                    .unwrap_or_else(|| RoleWord::new(generation, None, true).unwrap())
            };
            match authority.compare_exchange_weak(
                raw,
                released.0,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(observed) => raw = observed,
            }
        }
    }
}

pub struct Channel<H: Repr> {
    role: Arc<Role<H>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RoleAdmitError {
    Stale,
    Occupied,
    Terminal,
    Orphaned,
    Open,
}

impl<H: Repr> Clone for Channel<H> {
    fn clone(&self) -> Self {
        Self {
            role: self.role.clone(),
        }
    }
}

impl<H: Repr> Channel<H> {
    fn from_parts(
        mapped: Mapped<RcHeader<Duplex<H>>>,
        geometry: Geometry,
        id: Id<H>,
        index: usize,
        generation: usize,
        owner: u8,
    ) -> Self {
        let base = mapped.pointer().cast::<u8>();
        let slots = unsafe { NonNull::new_unchecked(base.as_ptr().add(geometry.slots).cast()) };
        Self {
            role: Arc::new(Role {
                id,
                routes: routes(&mapped, slots, geometry.capacity),
                mapped,
                capacity: geometry.capacity,
                one_lap: geometry.one_lap,
                index,
                generation,
                owner,
            }),
        }
    }

    pub fn id(&self) -> Id<H> {
        self.role.id
    }

    pub(crate) fn is_unique(&self) -> bool {
        Arc::strong_count(&self.role) == 1
    }

    pub(crate) fn peer_is_unowned(&self) -> bool {
        let peer = 1 - self.role.index;
        RoleWord(self.role.mapped.roles[peer].load(Ordering::Acquire))
            .parts()
            .1
            .is_none()
    }

    pub(crate) fn layout_id(&self) -> crate::schema::LayoutId {
        self.role.mapped.layout_id()
    }

    pub(crate) fn into_closed_mapped(self) -> Result<ClosedMap<H>, Self> {
        let authority = RoleWord(self.role.mapped.roles[self.role.index].load(Ordering::Acquire));
        let (_, owner, terminal) = authority.parts();
        if owner.is_some() || !terminal {
            return Err(self);
        }
        match Arc::try_unwrap(self.role) {
            Ok(role) => {
                // The unique role is terminal and unowned, so its Drop has no
                // authority to release. Move the mapping out without adding an
                // Option branch to every queue hot-path access.
                let role = ManuallyDrop::new(role);
                Ok((
                    unsafe { core::ptr::read(&role.mapped) },
                    role.index,
                    role.generation,
                    role.owner,
                ))
            }
            Err(role) => Err(Self { role }),
        }
    }

    pub(crate) fn closed(
        mapped: Mapped<RcHeader<Duplex<H>>>,
        geometry: Geometry,
        id: Id<H>,
        index: usize,
        generation: usize,
        owner: u8,
    ) -> Self {
        Self::from_parts(mapped, geometry, id, index, generation, owner)
    }

    pub(crate) fn creator(
        mapped: Mapped<RcHeader<Duplex<H>>>,
        geometry: Geometry,
        id: Id<H>,
    ) -> (Self, usize) {
        let owner = mapped.peer().slot();
        let generation = 1;
        mapped.roles[0].store(
            RoleWord::new(generation, Some(owner), false).unwrap().0,
            Ordering::Release,
        );
        mapped.roles[1].store(
            RoleWord::new(generation, None, false).unwrap().0,
            Ordering::Release,
        );
        (
            Self::from_parts(mapped, geometry, id, 0, generation, owner),
            generation,
        )
    }

    pub(crate) fn adopt(
        mapped: Mapped<RcHeader<Duplex<H>>>,
        port: &Port<H>,
    ) -> Result<Self, RoleAdmitError> {
        let index = port.role;
        let generation = port.generation;
        if index >= 2 {
            return Err(RoleAdmitError::Open);
        }
        let capacity =
            usize::try_from(mapped.layout_info().capacity).map_err(|_| RoleAdmitError::Open)?;
        let geometry = Duplex::<H>::geometry(capacity).map_err(|_| RoleAdmitError::Open)?;
        mapped
            .offset_of(mapped.pointer().cast(), geometry.layout.size())
            .map_err(|_| RoleAdmitError::Open)?;
        let owner = mapped.peer().slot();
        let authority = &mapped.roles[index];
        let expected = RoleWord::new(generation, None, false)
            .ok_or(RoleAdmitError::Stale)?
            .0;
        let claimed = RoleWord::new(generation, Some(owner), false)
            .ok_or(RoleAdmitError::Open)?
            .0;
        if let Err(observed) =
            authority.compare_exchange(expected, claimed, Ordering::AcqRel, Ordering::Acquire)
        {
            let (observed_generation, observed_owner, terminal) = RoleWord(observed).parts();
            return Err(if terminal {
                RoleAdmitError::Terminal
            } else if observed_generation != generation {
                RoleAdmitError::Stale
            } else if observed_owner.is_some() {
                RoleAdmitError::Occupied
            } else {
                RoleAdmitError::Open
            });
        }

        let (_, issuer, issuer_terminal) =
            RoleWord(mapped.roles[1 - index].load(Ordering::Acquire)).parts();
        if issuer_terminal || issuer.is_none() {
            let current = RoleWord(claimed);
            let released = current
                .next_unowned(false)
                .unwrap_or_else(|| RoleWord::new(generation, None, true).unwrap());
            let _ = authority.compare_exchange(
                claimed,
                released.0,
                Ordering::AcqRel,
                Ordering::Acquire,
            );
            return Err(RoleAdmitError::Orphaned);
        }

        Ok(Self::from_parts(
            mapped, geometry, port.id, index, generation, owner,
        ))
    }

    pub fn split(&self) -> (Tx<H>, Rx<H>) {
        (
            Tx(Endpoint {
                role: self.role.clone(),
                direction: self.role.index,
            }),
            Rx(Endpoint {
                role: self.role.clone(),
                direction: 1 - self.role.index,
            }),
        )
    }

    pub fn invite(&self) -> Result<Port<H>, InviteError> {
        let issuer = RoleWord(self.role.mapped.roles[self.role.index].load(Ordering::Acquire));
        let (issuer_generation, issuer_owner, issuer_terminal) = issuer.parts();
        if issuer_generation != self.role.generation
            || issuer_owner != Some(self.role.owner)
            || issuer_terminal
        {
            return Err(InviteError::Terminal);
        }
        let index = 1 - self.role.index;
        let authority = &self.role.mapped.roles[index];
        let raw = authority.load(Ordering::Acquire);
        let current = RoleWord(raw);
        let (generation, owner, terminal) = current.parts();
        if terminal {
            return Err(InviteError::Terminal);
        }
        if owner.is_some() {
            return Err(InviteError::Occupied);
        }
        let Some(next) = current.next_unowned(false) else {
            let retired = RoleWord::new(generation, None, true).unwrap();
            return match authority.compare_exchange(
                raw,
                retired.0,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => Err(InviteError::Retired),
                Err(_) => Err(InviteError::Busy),
            };
        };
        authority
            .compare_exchange(raw, next.0, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| InviteError::Busy)?;
        Ok(Port::new(self.role.id, index, next.parts().0))
    }

    pub(crate) fn close_with(
        &self,
        mut release: impl FnMut(&Token<H>) -> bool,
    ) -> Result<(), CloseError> {
        let authority = &self.role.mapped.roles[self.role.index];
        let mut raw = authority.load(Ordering::Acquire);
        loop {
            let current = RoleWord(raw);
            let (generation, owner, terminal) = current.parts();
            if generation != self.role.generation || owner != Some(self.role.owner) {
                return if terminal && owner.is_none() {
                    Ok(())
                } else {
                    Err(CloseError::Busy)
                };
            }
            if terminal {
                break;
            }
            let closing = RoleWord::new(generation, owner, true).unwrap();
            match authority.compare_exchange_weak(
                raw,
                closing.0,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => raw = observed,
            }
        }

        let handles = [
            Rx(Endpoint {
                role: self.role.clone(),
                direction: 0,
            }),
            Rx(Endpoint {
                role: self.role.clone(),
                direction: 1,
            }),
        ];
        for handle in &handles {
            handle.0.close_send();
        }
        for handle in &handles {
            loop {
                match Queue::claim(&handle.0) {
                    Ok(claim) => {
                        if !release(claim.item()) {
                            // The Queue remains authoritative until its storage
                            // evidence has been resolved and released.
                            core::mem::forget(claim);
                            return Err(CloseError::Evidence);
                        }
                        let _ = claim.take();
                    }
                    Err(ClaimError::Empty) => break,
                    Err(ClaimError::Busy | ClaimError::Closed) => {
                        return Err(CloseError::Busy);
                    }
                }
            }
            if !handle.0.send_closed() || !handle.0.is_empty() {
                return Err(CloseError::Busy);
            }
        }
        for handle in &handles {
            handle.0.close_recv();
            if !handle.0.recv_closed() {
                return Err(CloseError::Busy);
            }
        }

        let owned = RoleWord::new(self.role.generation, Some(self.role.owner), true)
            .unwrap()
            .0;
        let terminal = RoleWord::new(self.role.generation, None, true).unwrap().0;
        match authority.compare_exchange(owned, terminal, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => Ok(()),
            Err(observed) => {
                let (_, owner, terminal) = RoleWord(observed).parts();
                if terminal && owner.is_none() {
                    Ok(())
                } else {
                    Err(CloseError::Busy)
                }
            }
        }
    }
}

struct Endpoint<H: Repr> {
    role: Arc<Role<H>>,
    direction: usize,
}

pub struct Tx<H: Repr>(Endpoint<H>);

pub struct Rx<H: Repr>(Endpoint<H>);

impl<H: Repr> Clone for Endpoint<H> {
    fn clone(&self) -> Self {
        Self {
            role: self.role.clone(),
            direction: self.direction,
        }
    }
}

impl<H: Repr> Clone for Tx<H> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<H: Repr> Clone for Rx<H> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

/// A queue claim whose payload remains governed by Pool ownership.
///
/// Dropping this value deliberately retains the shared claim. Explicit
/// `adopt` or `discard` is required before the queue slot can be recycled.
#[must_use = "a received transfer must be adopted or discarded"]
pub struct Received<'a, H: Repr> {
    claim: Option<Claim<'a, Token<H>>>,
}

#[must_use]
pub struct TransferReserved<'a, H: Repr> {
    reserved: Reserved<'a, Token<H>>,
}

#[must_use]
pub struct TransferStaged<'a, 'p, H: Repr> {
    staged: Staged<'a, Token<H>>,
    allocation: crate::pool::Allocation<'p>,
}

impl<H: Repr> Tx<H> {
    pub fn reserve(&self) -> Result<TransferReserved<'_, H>, ReserveError> {
        Queue::reserve(&self.0).map(|reserved| TransferReserved { reserved })
    }

    #[expect(
        clippy::result_large_err,
        reason = "the uncommitted linear transfer must be returned inline without allocation"
    )]
    pub fn try_send<'p>(
        &self,
        value: crate::Transfer<'p, H>,
    ) -> Result<(), TrySendError<crate::Transfer<'p, H>>> {
        match self.reserve() {
            Ok(reserved) => {
                reserved.stage(value).publish();
                Ok(())
            }
            Err(ReserveError::Closed) => Err(TrySendError::Disconnected(value)),
            Err(ReserveError::Full) => Err(TrySendError::Full(value)),
            Err(ReserveError::Busy) => Err(TrySendError::Busy(value)),
        }
    }

    pub fn close(&self) {
        self.0.close_send();
    }

    pub fn is_close(&self) -> bool {
        self.0.field_state(self.0.send_field()) != 0
    }

    pub fn capacity(&self) -> usize {
        Queue::capacity(&self.0)
    }
}

impl<'a, H: Repr> TransferReserved<'a, H> {
    pub fn stage<'p>(self, value: crate::Transfer<'p, H>) -> TransferStaged<'a, 'p, H> {
        let (allocation, token) = value.into_parts();
        TransferStaged {
            staged: self.reserved.stage(token),
            allocation,
        }
    }
}

impl<'a, 'p, H: Repr> TransferStaged<'a, 'p, H> {
    pub fn publish(mut self) {
        assert!(
            self.allocation.detach().is_ok(),
            "linear transfer lost its Pool authority before publication"
        );
        self.staged.publish();
    }

    pub fn cancel(self) -> crate::Transfer<'p, H> {
        let token = self.staged.cancel();
        crate::Transfer::from_parts(self.allocation, token)
    }
}

impl<H: Repr> Rx<H> {
    pub fn claim(&self) -> Result<Received<'_, H>, ReceiveError> {
        match Queue::claim(&self.0) {
            Ok(claim) => Ok(Received { claim: Some(claim) }),
            Err(ClaimError::Empty) if self.0.terminal() => Err(ReceiveError::Closed),
            Err(ClaimError::Empty) => Err(ReceiveError::Empty),
            Err(ClaimError::Busy) => Err(ReceiveError::Busy),
            Err(ClaimError::Closed) => Err(ReceiveError::Closed),
        }
    }

    pub fn close(&self) {
        self.0.close_recv();
    }

    pub fn is_close(&self) -> bool {
        self.0.field_state(self.0.recv_field()) != 0
    }

    pub fn capacity(&self) -> usize {
        Queue::capacity(&self.0)
    }
}

impl<H: Repr> Queue for Endpoint<H> {
    type Item = Item<H>;

    fn header(&self) -> &Header {
        unsafe { self.role.route(self.direction).header.as_ref() }
    }

    fn buf(&self) -> &[Slot<Self::Item>] {
        unsafe {
            core::slice::from_raw_parts(
                self.role.route(self.direction).slots.as_ptr(),
                self.role.capacity,
            )
        }
    }

    fn lifecycle(&self) -> &AtomicU8 {
        &self.role.mapped.lifecycle
    }

    fn send_field(&self) -> u32 {
        self.role.route(self.direction).send_field
    }

    fn recv_field(&self) -> u32 {
        self.role.route(self.direction).recv_field
    }

    fn owner(&self) -> u8 {
        self.role.owner
    }

    fn one_lap(&self) -> usize {
        self.role.one_lap
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiveError {
    Empty,
    Busy,
    Closed,
}

pub enum AdoptError<'a, H: Repr> {
    Pool(Received<'a, H>),
    Span(Received<'a, H>),
    Type(Received<'a, H>),
    Owned(Received<'a, H>),
}

impl<H: Repr> core::fmt::Debug for AdoptError<'_, H> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Pool(_) => "Pool(..)",
            Self::Span(_) => "Span(..)",
            Self::Type(_) => "Type(..)",
            Self::Owned(_) => "Owned(..)",
        })
    }
}

impl<'a, H: Repr> AdoptError<'a, H> {
    pub fn into_received(self) -> Received<'a, H> {
        match self {
            Self::Pool(received)
            | Self::Span(received)
            | Self::Type(received)
            | Self::Owned(received) => received,
        }
    }

    fn from_pool(error: crate::pool::AdoptError, received: Received<'a, H>) -> Self {
        match error {
            crate::pool::AdoptError::Pool => Self::Pool(received),
            crate::pool::AdoptError::Span => Self::Span(received),
            crate::pool::AdoptError::Type => Self::Type(received),
            crate::pool::AdoptError::Owned => Self::Owned(received),
        }
    }
}

impl<'a, H: Repr> Received<'a, H> {
    fn claim(&self) -> &Claim<'a, Token<H>> {
        self.claim.as_ref().unwrap()
    }

    fn take_claim(mut self) -> Claim<'a, Token<H>> {
        self.claim.take().unwrap()
    }

    pub fn adopt<'p, T: Repr + crate::token::Shape + ?Sized>(
        self,
        pool: crate::PoolRef<'p>,
    ) -> Result<(H, crate::Block<'p, T>), AdoptError<'a, H>> {
        let block = match pool.adopt::<H, T>(self.claim().item()) {
            Ok(block) => block,
            Err(error) => return Err(AdoptError::from_pool(error, self)),
        };
        let transfer = self.take_claim().take();
        Ok((transfer.header, block))
    }

    pub fn discard(self, pool: crate::PoolRef<'_>) -> Result<H, AdoptError<'a, H>> {
        let allocation = match pool.takeover(self.claim().item()) {
            Ok(allocation) => allocation,
            Err(error) => return Err(AdoptError::from_pool(error, self)),
        };
        let transfer = self.take_claim().take();
        drop(allocation);
        Ok(transfer.header)
    }
}

impl<H: Repr> Drop for Received<'_, H> {
    fn drop(&mut self) {
        if let Some(claim) = self.claim.take() {
            core::mem::forget(claim);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Channel, Duplex, GeometryError, Id, Port, RoleWord, Rx, Tx, recover_role};
    use crate::{
        msg::Repr,
        schema::{SchemaId, SchemaKey},
    };

    #[repr(C, align(64))]
    struct Aligned([u8; 64]);

    unsafe impl Repr for Aligned {
        const SCHEMA: SchemaKey = SchemaKey::new(SchemaId(0x414c_4947_4e45_4401), 1);
    }

    struct NonClone;

    unsafe impl Repr for NonClone {
        const SCHEMA: SchemaKey = SchemaKey::new(SchemaId(0x4e4f_4e43_4c4f_4e45), 1);
    }

    fn parameterless_split<H: Repr>(channel: &Channel<H>) -> (Tx<H>, Rx<H>) {
        channel.split()
    }

    #[test]
    fn channel_split_has_no_runtime_side_argument() {
        let _ = parameterless_split::<()>;
        fn cloneable<T: Clone>() {}
        cloneable::<Tx<NonClone>>();
        cloneable::<Rx<NonClone>>();
    }

    #[test]
    fn port_wire_reconstruction_rejects_invalid_authority_parts() {
        let id = Id::<()>::new(crate::schema::RegionId::new(1, 2), 3, 4, 5, 8);
        assert!(Port::from_parts(id, 0, 7).is_some());
        assert!(Port::from_parts(id, 1, 7).is_some());
        assert!(Port::from_parts(id, 2, 7).is_none());
        assert!(Port::from_parts(id, 0, 0).is_none());
        assert!(Port::from_parts(id, 0, RoleWord::MAX_GENERATION + 1).is_none());
    }

    #[test]
    fn dynamic_geometry_rejects_invalid_capacity() {
        assert!(matches!(
            Duplex::<()>::geometry(0),
            Err(GeometryError::ZeroCapacity)
        ));
        assert!(matches!(
            Duplex::<()>::geometry(usize::MAX),
            Err(GeometryError::Overflow)
        ));
    }

    #[test]
    fn dynamic_geometry_accepts_non_power_of_two_capacity() {
        let one = Duplex::<()>::geometry(1).unwrap();
        let three = Duplex::<()>::geometry(3).unwrap();
        assert!(three.layout.size() > one.layout.size());
        assert_eq!(three.layout.size() % three.layout.align(), 0);
        assert_eq!(three.capacity, 3);
        assert_eq!(three.one_lap, 4);

        let aligned = Duplex::<Aligned>::geometry(3).unwrap();
        assert!(aligned.layout.align() >= 64);
        assert_eq!(aligned.slots % 64, 0);
    }

    #[test]
    fn role_word_round_trips_canonical_authority() {
        let open = RoleWord::new(7, None, false).unwrap();
        assert_eq!(open.parts(), (7, None, false));

        let owned = RoleWord::new(7, Some(3), false).unwrap();
        assert_eq!(owned.parts(), (7, Some(3), false));

        let closing = RoleWord::new(7, Some(3), true).unwrap();
        assert_eq!(closing.parts(), (7, Some(3), true));

        let terminal = RoleWord::new(7, None, true).unwrap();
        assert_eq!(RoleWord(terminal.0).parts(), (7, None, true));
    }

    #[test]
    fn role_generation_never_wraps() {
        let last = RoleWord::new(RoleWord::MAX_GENERATION, Some(3), false).unwrap();
        assert!(last.next_unowned(false).is_none());
        assert!(RoleWord::new(RoleWord::MAX_GENERATION + 1, None, false).is_none());
    }

    #[test]
    fn recovery_releases_only_the_dead_owner_after_repair() {
        use core::sync::atomic::{AtomicUsize, Ordering};

        let active = AtomicUsize::new(RoleWord::new(7, Some(3), false).unwrap().0);
        recover_role(&active, 3);
        assert_eq!(
            RoleWord(active.load(Ordering::Relaxed)).parts(),
            (8, None, false)
        );

        let closing = AtomicUsize::new(RoleWord::new(7, Some(3), true).unwrap().0);
        recover_role(&closing, 3);
        assert_eq!(
            RoleWord(closing.load(Ordering::Relaxed)).parts(),
            (7, None, true)
        );

        let live = AtomicUsize::new(RoleWord::new(7, Some(4), false).unwrap().0);
        recover_role(&live, 3);
        assert_eq!(
            RoleWord(live.load(Ordering::Relaxed)).parts(),
            (7, Some(4), false)
        );
    }

    #[test]
    fn duplex_initializes_both_roles_open_unowned() {
        use core::sync::atomic::Ordering;

        let duplex = Duplex::<()>::blank();
        for role in &duplex.roles {
            let word = RoleWord(role.load(Ordering::Relaxed));
            assert_eq!(word.parts(), (0, None, false));
        }
    }
}
