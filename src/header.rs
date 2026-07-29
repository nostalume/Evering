use core::{
    mem,
    ops::Deref,
    ptr,
    sync::atomic::{AtomicU8, AtomicU16, AtomicUsize, Ordering},
};

use crate::schema::{
    LayoutContext, LayoutId, LayoutInfo, RegionId, SchemaId, SchemaKey, SharedSchema, schema_id,
};

pub type Magic = u16;
type AtomicMagic = AtomicU16;

#[cfg(test)]
static CLAIM_TEST_TARGET: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static CLAIM_TEST_WAITERS: AtomicUsize = AtomicUsize::new(0);

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Uninitialized = 0,
    Initializing = 1,
    Initialized = 2,
    Corrupted = 3, // optional
}

impl Status {
    #[inline]
    pub const fn from_u8(v: u8) -> Status {
        match v {
            0 => Status::Uninitialized,
            1 => Status::Initializing,
            2 => Status::Initialized,
            _ => Status::Corrupted,
        }
    }
}

/// A process-shared layout body with an independently recorded schema and information value.
///
/// # Safety
///
/// Implementors must give the declared schema a stable shared representation.
/// `init` must fully initialize `destination` without first reading it.
pub unsafe trait Layout: SharedSchema + Sized {
    type Config: Copy;
    type Info: LayoutInfo;

    const MAGIC: Magic;

    fn info(conf: &Self::Config, ctx: LayoutContext) -> Self::Info;

    unsafe fn init(destination: *mut Self, conf: Self::Config) -> Status;
    fn attach(&self, conf: &Self::Config) -> Status;
}

#[doc(hidden)]
pub trait AdmitLayout: Layout {
    unsafe fn admit(
        pointer: *mut Self,
        conf: Self::Config,
        ctx: LayoutContext,
        slot: Option<u8>,
    ) -> Result<(), AdmitError>;

    unsafe fn leave(pointer: *mut Self, slot: u8);
}

#[repr(C)]
pub struct RcHeader<T: Layout> {
    magic: AtomicMagic,
    status: AtomicU8,
    members: AtomicUsize,
    revision: u32,
    schema: SchemaId,
    region: RegionId,
    offset: u64,
    header_size: u64,
    header_align: u64,
    info_schema: SchemaId,
    info_revision: u32,
    info_size: u64,
    info_align: u64,
    body_size: u64,
    body_align: u64,
    info: T::Info,
    pub inner: T,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutField {
    State,
    Magic,
    Revision,
    Schema,
    Region,
    Offset,
    HeaderSize,
    HeaderAlign,
    InfoSchema,
    InfoRevision,
    InfoSize,
    InfoAlign,
    Size,
    Align,
    Info,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdmitError {
    Contention,
    Closed,
    DuplicateMember,
    InvalidHeader,
    Mismatch(LayoutField),
}

const CLOSED: usize = 1usize << (usize::BITS - 1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseError {
    Contention,
    MissingMember,
    Closed,
}

impl<T: Layout + core::fmt::Debug> core::fmt::Debug for RcHeader<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let status = Status::from_u8(self.status.load(Ordering::Relaxed));
        f.debug_struct("RcHeader")
            .field("magic", &self.magic)
            .field("status", &status)
            .field("inner", &self.inner)
            .finish()
    }
}

impl<T: Layout> const Deref for RcHeader<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T: Layout> RcHeader<T> {
    #[inline]
    pub fn status(&self) -> Status {
        Status::from_u8(self.status.load(Ordering::Acquire))
    }

    #[inline]
    pub fn layout_id(&self) -> LayoutId {
        debug_assert_eq!(self.status(), Status::Initialized);
        LayoutId {
            region: self.region,
            offset: self.offset,
        }
    }

    #[inline]
    pub(crate) const fn layout_info(&self) -> T::Info {
        self.info
    }

    pub(crate) fn close_unique(&self, slot: u8) -> Result<(), CloseError> {
        let bit = 1usize << slot;
        self.members
            .compare_exchange(bit, CLOSED, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|observed| {
                if observed & CLOSED != 0 {
                    CloseError::Closed
                } else if observed & bit == 0 {
                    CloseError::MissingMember
                } else {
                    CloseError::Contention
                }
            })
    }

    pub(crate) fn recover_member(&self, slot: u8) -> bool {
        let bit = 1usize << slot;
        self.members.fetch_and(!bit, Ordering::AcqRel) & bit != 0
    }

    pub(crate) fn has_member(&self, slot: u8) -> bool {
        self.members.load(Ordering::Acquire) & (1usize << slot) != 0
    }

    pub(crate) unsafe fn published_region_at(pointer: *const Self) -> Option<RegionId> {
        let status = unsafe { &*ptr::addr_of!((*pointer).status) };
        (Status::from_u8(status.load(Ordering::Acquire)) == Status::Initialized)
            .then(|| unsafe { ptr::addr_of!((*pointer).region).read() })
    }
}

pub(crate) unsafe fn closed_at(pointer: *const u8) -> bool {
    let pointer = pointer.cast::<RcHeader<()>>();
    let members = unsafe { &*ptr::addr_of!((*pointer).members) };
    members.load(Ordering::Acquire) & CLOSED != 0
}

unsafe impl<T: Layout> Layout for RcHeader<T> {
    type Config = T::Config;
    type Info = T::Info;

    const MAGIC: Magic = T::MAGIC;

    fn info(conf: &Self::Config, ctx: LayoutContext) -> Self::Info {
        T::info(conf, ctx)
    }

    unsafe fn init(_: *mut Self, _: Self::Config) -> Status {
        unreachable!("RcHeader initialization requires layout context")
    }

    fn attach(&self, _: &Self::Config) -> Status {
        unreachable!("RcHeader attachment requires layout context")
    }
}

impl<T: Layout> SharedSchema for RcHeader<T> {
    const SCHEMA: SchemaKey = T::SCHEMA;
}

impl<T: Layout> AdmitLayout for RcHeader<T> {
    unsafe fn admit(
        pointer: *mut Self,
        conf: Self::Config,
        ctx: LayoutContext,
        slot: Option<u8>,
    ) -> Result<(), AdmitError> {
        let status = unsafe { &*ptr::addr_of!((*pointer).status) };
        loop {
            match Status::from_u8(status.load(Ordering::Acquire)) {
                Status::Uninitialized if !ctx.allow_init => {
                    return Err(AdmitError::Mismatch(LayoutField::State));
                }
                Status::Uninitialized => {
                    #[cfg(test)]
                    if CLAIM_TEST_TARGET.load(Ordering::Relaxed) == pointer.addr() {
                        CLAIM_TEST_WAITERS.fetch_add(1, Ordering::Relaxed);
                        while CLAIM_TEST_WAITERS.load(Ordering::Acquire) != 2 {
                            std::thread::yield_now();
                        }
                    }
                    if status
                        .compare_exchange(
                            Status::Uninitialized as u8,
                            Status::Initializing as u8,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_err()
                    {
                        #[cfg(test)]
                        while CLAIM_TEST_TARGET.load(Ordering::Relaxed) == pointer.addr()
                            && Status::from_u8(status.load(Ordering::Acquire))
                                == Status::Initializing
                        {
                            std::thread::yield_now();
                        }
                        continue;
                    }
                    break;
                }
                Status::Initializing => return Err(AdmitError::Contention),
                Status::Corrupted => return Err(AdmitError::InvalidHeader),
                Status::Initialized => {
                    if let Some(field) = unsafe { Self::mismatch_at(pointer, &conf, ctx) } {
                        return Err(AdmitError::Mismatch(field));
                    }
                    let mut membership = unsafe { Self::claim(pointer, slot) }?;
                    return match T::attach(unsafe { &*ptr::addr_of!((*pointer).inner) }, &conf) {
                        Status::Initialized => {
                            membership.disarm();
                            Ok(())
                        }
                        Status::Initializing => Err(AdmitError::Contention),
                        _ => Err(AdmitError::InvalidHeader),
                    };
                }
            }
        }

        let mut claim = InitClaim::new(status);
        let info = T::info(&conf, ctx);
        let bit = slot.map_or(0, |slot| 1usize << slot);
        unsafe {
            ptr::addr_of_mut!((*pointer).revision).write(T::SCHEMA.revision);
            ptr::addr_of_mut!((*pointer).members).write(AtomicUsize::new(bit));
            ptr::addr_of_mut!((*pointer).schema).write(T::SCHEMA.id);
            ptr::addr_of_mut!((*pointer).region).write(ctx.region);
            ptr::addr_of_mut!((*pointer).offset).write(ctx.offset);
            ptr::addr_of_mut!((*pointer).header_size).write(mem::size_of::<Self>() as u64);
            ptr::addr_of_mut!((*pointer).header_align).write(mem::align_of::<Self>() as u64);
            ptr::addr_of_mut!((*pointer).info_schema).write(T::Info::SCHEMA.id);
            ptr::addr_of_mut!((*pointer).info_revision).write(T::Info::SCHEMA.revision);
            ptr::addr_of_mut!((*pointer).info_size).write(mem::size_of::<T::Info>() as u64);
            ptr::addr_of_mut!((*pointer).info_align).write(mem::align_of::<T::Info>() as u64);
            ptr::addr_of_mut!((*pointer).body_size).write(mem::size_of::<T>() as u64);
            ptr::addr_of_mut!((*pointer).body_align).write(mem::align_of::<T>() as u64);
            ptr::addr_of_mut!((*pointer).info).write(info);
        }
        let members = unsafe { &*ptr::addr_of!((*pointer).members) };
        let mut membership = Membership::new(members, bit);
        if unsafe { T::init(ptr::addr_of_mut!((*pointer).inner), conf) } != Status::Initialized {
            return Err(AdmitError::InvalidHeader);
        }
        let magic = unsafe { &*ptr::addr_of!((*pointer).magic) };
        magic.store(T::MAGIC, Ordering::Release);
        status.store(Status::Initialized as u8, Ordering::Release);
        membership.disarm();
        claim.disarm();
        Ok(())
    }

    unsafe fn leave(pointer: *mut Self, slot: u8) {
        let members = unsafe { &*ptr::addr_of!((*pointer).members) };
        members.fetch_and(!(1usize << slot), Ordering::AcqRel);
    }
}

impl<T: Layout> RcHeader<T> {
    unsafe fn claim(pointer: *mut Self, slot: Option<u8>) -> Result<Membership, AdmitError> {
        let members = unsafe { &*ptr::addr_of!((*pointer).members) };
        let Some(slot) = slot else {
            return Ok(Membership::new(members, 0));
        };
        let bit = 1usize << slot;
        let observed = members.load(Ordering::Acquire);
        if observed & CLOSED != 0 {
            return Err(AdmitError::Closed);
        }
        if observed & bit != 0 {
            return Err(AdmitError::DuplicateMember);
        }
        members
            .compare_exchange(
                observed,
                observed | bit,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| Membership::new(members, bit))
            .map_err(|_| AdmitError::Contention)
    }
}

impl<T: Layout> RcHeader<T> {
    pub(crate) unsafe fn inspect_at(
        pointer: *const Self,
        ctx: LayoutContext,
    ) -> Result<T::Info, AdmitError> {
        let status = unsafe { &*ptr::addr_of!((*pointer).status) };
        if Status::from_u8(status.load(Ordering::Acquire)) != Status::Initialized {
            return Err(AdmitError::Mismatch(LayoutField::State));
        }
        let magic = unsafe { &*ptr::addr_of!((*pointer).magic) };
        if magic.load(Ordering::Acquire) != T::MAGIC {
            return Err(AdmitError::Mismatch(LayoutField::Magic));
        }
        macro_rules! check {
            ($member:ident, $expected:expr, $field:ident) => {
                if unsafe { ptr::addr_of!((*pointer).$member).read() } != $expected {
                    return Err(AdmitError::Mismatch(LayoutField::$field));
                }
            };
        }
        check!(revision, T::SCHEMA.revision, Revision);
        check!(schema, T::SCHEMA.id, Schema);
        check!(region, ctx.region, Region);
        check!(offset, ctx.offset, Offset);
        check!(header_size, mem::size_of::<Self>() as u64, HeaderSize);
        check!(header_align, mem::align_of::<Self>() as u64, HeaderAlign);
        check!(info_schema, T::Info::SCHEMA.id, InfoSchema);
        check!(info_revision, T::Info::SCHEMA.revision, InfoRevision);
        check!(info_size, mem::size_of::<T::Info>() as u64, InfoSize);
        check!(info_align, mem::align_of::<T::Info>() as u64, InfoAlign);
        check!(body_size, mem::size_of::<T>() as u64, Size);
        check!(body_align, mem::align_of::<T>() as u64, Align);
        Ok(unsafe { ptr::addr_of!((*pointer).info).read() })
    }

    unsafe fn mismatch_at(
        pointer: *const Self,
        conf: &T::Config,
        ctx: LayoutContext,
    ) -> Option<LayoutField> {
        let info = match unsafe { Self::inspect_at(pointer, ctx) } {
            Ok(info) => info,
            Err(AdmitError::Mismatch(field)) => return Some(field),
            Err(_) => return Some(LayoutField::State),
        };
        if info != T::info(conf, ctx) {
            return Some(LayoutField::Info);
        }
        None
    }
}

struct Membership {
    members: *const AtomicUsize,
    bit: usize,
}

impl Membership {
    const fn new(members: &AtomicUsize, bit: usize) -> Self {
        Self { members, bit }
    }

    const fn disarm(&mut self) {
        self.bit = 0;
    }
}

impl Drop for Membership {
    fn drop(&mut self) {
        if self.bit != 0 {
            unsafe { &*self.members }.fetch_and(!self.bit, Ordering::AcqRel);
        }
    }
}

struct InitClaim<'a>(&'a AtomicU8, bool);

impl<'a> InitClaim<'a> {
    fn new(status: &'a AtomicU8) -> Self {
        Self(status, true)
    }

    const fn disarm(&mut self) {
        self.1 = false;
    }
}

impl Drop for InitClaim<'_> {
    fn drop(&mut self) {
        if self.1 {
            self.0.store(Status::Corrupted as u8, Ordering::Release);
        }
    }
}

unsafe impl Layout for () {
    type Config = ();
    type Info = ();

    const MAGIC: Magic = 0x0;

    fn info(_: &(), _: LayoutContext) {}

    unsafe fn init(destination: *mut Self, _conf: ()) -> Status {
        unsafe { destination.write(()) };
        Status::Initialized
    }

    fn attach(&self, _conf: &()) -> Status {
        Status::Initialized
    }
}

impl AdmitLayout for () {
    unsafe fn admit(
        _: *mut Self,
        _: Self::Config,
        _: LayoutContext,
        _: Option<u8>,
    ) -> Result<(), AdmitError> {
        Ok(())
    }

    unsafe fn leave(_: *mut Self, _: u8) {}
}

pub(crate) const PARTICIPANT_CAPACITY: usize = usize::BITS as usize - 1;
const MEMBER_STATE_BITS: usize = 2;
const MEMBER_STATE_MASK: usize = (1 << MEMBER_STATE_BITS) - 1;
const MEMBER_FREE: usize = 0;
const MEMBER_LIVE: usize = 1;
const MEMBER_DEAD: usize = 2;
const MEMBER_RETIRED: usize = 3;
const MEMBER_GENERATION_MAX: usize = usize::MAX >> MEMBER_STATE_BITS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Member {
    pub(crate) slot: u8,
    pub(crate) generation: usize,
}

#[repr(C)]
#[derive(Debug)]
pub struct Root {
    members: [AtomicUsize; PARTICIPANT_CAPACITY],
}

pub type RootHeader = RcHeader<Root>;

impl SharedSchema for Root {
    const SCHEMA: SchemaKey = SchemaKey::new(schema_id("evering.root"), 1);
}

unsafe impl Layout for Root {
    type Config = ();
    type Info = ();

    const MAGIC: Magic = 0xABCD;

    fn info(_: &(), _: LayoutContext) {}

    #[inline]
    unsafe fn init(destination: *mut Self, _conf: Self::Config) -> Status {
        unsafe {
            destination.write(Self {
                members: [const { AtomicUsize::new(0) }; PARTICIPANT_CAPACITY],
            })
        };
        Status::Initialized
    }

    #[inline]
    fn attach(&self, _conf: &()) -> Status {
        Status::Initialized
    }
}

impl Root {
    pub(crate) fn join(&self) -> Option<Member> {
        for (slot, state) in self.members.iter().enumerate() {
            let observed = state.load(Ordering::Acquire);
            if observed & MEMBER_STATE_MASK != MEMBER_FREE {
                continue;
            }
            let prior = observed >> MEMBER_STATE_BITS;
            if prior == MEMBER_GENERATION_MAX {
                let _ = state.compare_exchange(
                    observed,
                    Self::word(
                        Member {
                            slot: slot as u8,
                            generation: prior,
                        },
                        MEMBER_RETIRED,
                    ),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
                continue;
            }
            let generation = prior + 1;
            let live = (generation << MEMBER_STATE_BITS) | MEMBER_LIVE;
            if state
                .compare_exchange(observed, live, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Some(Member {
                    slot: slot as u8,
                    generation,
                });
            }
        }
        None
    }

    fn word(member: Member, state: usize) -> usize {
        (member.generation << MEMBER_STATE_BITS) | state
    }

    fn member(&self, member: Member) -> Option<&AtomicUsize> {
        self.members.get(member.slot as usize)
    }

    pub(crate) fn leave(&self, member: Member) -> bool {
        let Some(state) = self.member(member) else {
            return false;
        };
        state
            .compare_exchange(
                Self::word(member, MEMBER_LIVE),
                Self::word(member, MEMBER_FREE),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(crate) fn mark_dead(&self, member: Member) -> bool {
        let Some(state) = self.member(member) else {
            return false;
        };
        state
            .compare_exchange(
                Self::word(member, MEMBER_LIVE),
                Self::word(member, MEMBER_DEAD),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(crate) fn is_dead(&self, member: Member) -> bool {
        self.member(member)
            .is_some_and(|state| state.load(Ordering::Acquire) == Self::word(member, MEMBER_DEAD))
    }

    pub(crate) fn release_dead(&self, member: Member) -> bool {
        let Some(state) = self.member(member) else {
            return false;
        };
        let next = if member.generation == MEMBER_GENERATION_MAX {
            Self::word(member, MEMBER_RETIRED)
        } else {
            Self::word(member, MEMBER_FREE)
        };
        state
            .compare_exchange(
                Self::word(member, MEMBER_DEAD),
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

#[cfg(test)]
mod admission_tests {
    use super::{AdmitError, AdmitLayout, Layout, LayoutField, RcHeader as Header, Root, Status};
    use crate::schema::{
        LayoutContext, RegionId, SchemaKey, SharedSchema, compose_const, schema_id,
    };
    use core::panic::AssertUnwindSafe;
    use core::sync::atomic::{AtomicUsize, Ordering};
    use core::{mem::MaybeUninit, ptr};

    struct Reject;

    impl SharedSchema for Reject {
        const SCHEMA: SchemaKey = compose_const(SchemaKey::new(schema_id("test.reject"), 1), 1);
    }

    unsafe impl Layout for Reject {
        type Config = (&'static AtomicUsize, bool);
        type Info = ();
        const MAGIC: super::Magic = 0x5151;

        fn info(_: &Self::Config, _: LayoutContext) {}

        unsafe fn init(destination: *mut Self, conf: Self::Config) -> Status {
            conf.0.fetch_add(1, Ordering::Relaxed);
            unsafe { destination.write(Self) };
            Status::Initialized
        }

        fn attach(&self, conf: &Self::Config) -> Status {
            assert!(conf.1, "common mismatch reached typed attachment");
            Status::Corrupted
        }
    }

    #[test]
    fn initialized_layout_propagates_attachment_rejection_without_reinitializing() {
        static INITS: AtomicUsize = AtomicUsize::new(0);
        INITS.store(0, Ordering::Relaxed);
        let mut storage = MaybeUninit::<Header<Reject>>::zeroed();
        let header = storage.as_mut_ptr();

        let ctx = LayoutContext {
            region: RegionId::new(1, 2),
            offset: 0,
            allow_init: true,
        };
        assert_eq!(
            unsafe {
                <Header<Reject> as AdmitLayout>::admit(header, (&INITS, false), ctx, Some(2))
            },
            Ok(())
        );
        assert_eq!(INITS.load(Ordering::Relaxed), 1);
        assert_eq!(
            unsafe { <Header<Reject> as AdmitLayout>::admit(header, (&INITS, true), ctx, Some(3)) },
            Err(AdmitError::InvalidHeader)
        );
        assert_eq!(
            unsafe { <Header<Reject> as AdmitLayout>::admit(header, (&INITS, true), ctx, Some(3)) },
            Err(AdmitError::InvalidHeader)
        );
        assert_eq!(INITS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn common_record_mismatch_prevents_typed_attachment() {
        static INITS: AtomicUsize = AtomicUsize::new(0);
        INITS.store(0, Ordering::Relaxed);
        let mut storage = MaybeUninit::<Header<Reject>>::zeroed();
        let header = storage.as_mut_ptr();
        let first = LayoutContext {
            region: RegionId::new(3, 4),
            offset: 8,
            allow_init: true,
        };
        let moved = LayoutContext {
            offset: 16,
            allow_init: false,
            ..first
        };

        assert_eq!(
            unsafe { <Header<Reject> as AdmitLayout>::admit(header, (&INITS, false), first, None) },
            Ok(())
        );
        assert_eq!(
            unsafe { Header::<Reject>::mismatch_at(header, &(&INITS, false), moved) },
            Some(LayoutField::Offset)
        );
        assert_eq!(
            unsafe { <Header<Reject> as AdmitLayout>::admit(header, (&INITS, false), moved, None) },
            Err(AdmitError::Mismatch(LayoutField::Offset))
        );
    }

    struct PanicInfo;

    impl SharedSchema for PanicInfo {
        const SCHEMA: SchemaKey = SchemaKey::new(schema_id("test.panic-info"), 1);
    }

    unsafe impl Layout for PanicInfo {
        type Config = ();
        type Info = ();
        const MAGIC: super::Magic = 0x6161;

        fn info(_: &(), _: LayoutContext) {
            panic!("expected information panic")
        }

        unsafe fn init(_: *mut Self, _: ()) -> Status {
            unreachable!()
        }

        fn attach(&self, _: &()) -> Status {
            Status::Initialized
        }
    }

    #[test]
    fn unwind_after_claim_publishes_corrupted() {
        let mut storage = MaybeUninit::<Header<PanicInfo>>::zeroed();
        let pointer = storage.as_mut_ptr();
        let ctx = LayoutContext {
            region: RegionId::new(7, 8),
            offset: 0,
            allow_init: true,
        };
        let panic = std::panic::catch_unwind(AssertUnwindSafe(|| unsafe {
            <Header<PanicInfo> as AdmitLayout>::admit(pointer, (), ctx, None)
        }));
        assert!(panic.is_err());
        assert_eq!(unsafe { &*pointer }.status(), Status::Corrupted);
    }

    struct FailedInit;

    impl SharedSchema for FailedInit {
        const SCHEMA: SchemaKey = SchemaKey::new(schema_id("test.failed-init"), 1);
    }

    unsafe impl Layout for FailedInit {
        type Config = ();
        type Info = ();
        const MAGIC: super::Magic = 0x6262;
        fn info(_: &(), _: LayoutContext) {}
        unsafe fn init(_: *mut Self, _: ()) -> Status {
            Status::Corrupted
        }
        fn attach(&self, _: &()) -> Status {
            unreachable!()
        }
    }

    #[test]
    fn returned_initialization_failure_publishes_corrupted() {
        let mut storage = MaybeUninit::<Header<FailedInit>>::zeroed();
        let pointer = storage.as_mut_ptr();
        let ctx = LayoutContext {
            region: RegionId::new(8, 9),
            offset: 0,
            allow_init: true,
        };
        assert_eq!(
            unsafe { <Header<FailedInit> as AdmitLayout>::admit(pointer, (), ctx, None) },
            Err(AdmitError::InvalidHeader)
        );
        assert_eq!(unsafe { &*pointer }.status(), Status::Corrupted);
    }

    #[test]
    fn root_attachment_revalidates_without_joining_a_participant() {
        let mut storage = MaybeUninit::<Header<Root>>::zeroed();
        let pointer = storage.as_mut_ptr();
        let ctx = LayoutContext {
            region: RegionId::new(9, 10),
            offset: 0,
            allow_init: true,
        };
        assert_eq!(
            unsafe { <Header<Root> as AdmitLayout>::admit(pointer, (), ctx, None) },
            Ok(())
        );
        let first = unsafe { (*pointer).inner.join() }.unwrap();
        assert_eq!(
            unsafe {
                <Header<Root> as AdmitLayout>::admit(
                    pointer,
                    (),
                    LayoutContext {
                        allow_init: false,
                        ..ctx
                    },
                    None,
                )
            },
            Ok(())
        );
        assert_eq!(
            unsafe {
                <Header<Root> as AdmitLayout>::admit(
                    pointer,
                    (),
                    LayoutContext {
                        region: RegionId::new(11, 12),
                        allow_init: false,
                        ..ctx
                    },
                    None,
                )
            },
            Err(AdmitError::Mismatch(LayoutField::Region))
        );
        assert!(unsafe { (*pointer).inner.leave(first) });
    }

    #[test]
    fn participant_generation_blocks_stale_owner_and_dead_reuse() {
        let root = Root {
            members: [const { AtomicUsize::new(0) }; super::PARTICIPANT_CAPACITY],
        };
        let first = root.join().unwrap();
        let others = (1..super::PARTICIPANT_CAPACITY)
            .map(|_| root.join().unwrap())
            .collect::<Vec<_>>();
        assert!(root.join().is_none());

        assert!(root.mark_dead(first));
        assert!(root.is_dead(first));
        assert!(!root.leave(first));
        assert!(root.join().is_none());
        assert!(root.release_dead(first));

        let replacement = root.join().unwrap();
        assert_eq!(replacement.slot, first.slot);
        assert_ne!(replacement.generation, first.generation);
        assert!(!root.leave(first));
        assert!(root.leave(replacement));
        for member in others {
            assert!(root.leave(member));
        }
    }

    fn race_root(second_region: RegionId) -> [Result<(), AdmitError>; 2] {
        let storage = Box::into_raw(Box::new(MaybeUninit::<Header<Root>>::zeroed()));
        let pointer = storage.cast::<Header<Root>>();
        let address = pointer.addr();
        let first = LayoutContext {
            region: RegionId::new(21, 22),
            offset: 0,
            allow_init: true,
        };
        super::CLAIM_TEST_WAITERS.store(0, Ordering::Relaxed);
        super::CLAIM_TEST_TARGET.store(address, Ordering::Release);
        let spawn = |ctx| {
            std::thread::spawn(move || unsafe {
                <Header<Root> as AdmitLayout>::admit(address as *mut _, (), ctx, None)
            })
        };
        let left = spawn(first);
        let right = spawn(LayoutContext {
            region: second_region,
            ..first
        });
        let results = [left.join().unwrap(), right.join().unwrap()];
        super::CLAIM_TEST_TARGET.store(0, Ordering::Release);
        unsafe { drop(Box::from_raw(storage)) };
        results
    }

    #[test]
    fn initialization_cas_loser_revalidates_and_attaches_once() {
        let same = race_root(RegionId::new(21, 22));
        assert_eq!(same, [Ok(()), Ok(())]);

        let conflict = race_root(RegionId::new(23, 24));
        assert_eq!(
            conflict
                .iter()
                .filter(|result| matches!(result, Err(AdmitError::Mismatch(LayoutField::Region))))
                .count(),
            1
        );
    }

    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct WideInfo([u64; 2]);

    impl SharedSchema for WideInfo {
        const SCHEMA: SchemaKey = SchemaKey::new(schema_id("test.wide-info"), 1);
    }

    unsafe impl crate::LayoutInfo for WideInfo {}

    struct Narrow;
    struct Wide;

    impl SharedSchema for Narrow {
        const SCHEMA: SchemaKey = SchemaKey::new(schema_id("test.same-body"), 1);
    }

    impl SharedSchema for Wide {
        const SCHEMA: SchemaKey = Narrow::SCHEMA;
    }

    unsafe impl Layout for Narrow {
        type Config = ();
        type Info = ();
        const MAGIC: super::Magic = 0x7171;
        fn info(_: &(), _: LayoutContext) {}
        unsafe fn init(destination: *mut Self, _: ()) -> Status {
            unsafe { destination.write(Self) };
            Status::Initialized
        }
        fn attach(&self, _: &()) -> Status {
            Status::Initialized
        }
    }

    unsafe impl Layout for Wide {
        type Config = ();
        type Info = WideInfo;
        const MAGIC: super::Magic = 0x7171;
        fn info(_: &(), _: LayoutContext) -> WideInfo {
            WideInfo([0; 2])
        }
        unsafe fn init(destination: *mut Self, _: ()) -> Status {
            unsafe { destination.write(Self) };
            Status::Initialized
        }
        fn attach(&self, _: &()) -> Status {
            Status::Initialized
        }
    }

    #[test]
    fn information_extent_is_validated_before_typed_information() {
        let mut storage = MaybeUninit::<Header<Narrow>>::zeroed();
        let pointer = storage.as_mut_ptr();
        let ctx = LayoutContext {
            region: RegionId::new(13, 14),
            offset: 0,
            allow_init: true,
        };
        assert_eq!(
            unsafe { <Header<Narrow> as AdmitLayout>::admit(pointer, (), ctx, None) },
            Ok(())
        );
        assert_eq!(
            unsafe {
                <Header<Wide> as AdmitLayout>::admit(
                    pointer.cast(),
                    (),
                    LayoutContext {
                        allow_init: false,
                        ..ctx
                    },
                    None,
                )
            },
            Err(AdmitError::Mismatch(LayoutField::HeaderSize))
        );
    }

    #[test]
    fn admitted_header_exposes_its_layout_identity() {
        let mut storage = MaybeUninit::<Header<Root>>::zeroed();
        let pointer = storage.as_mut_ptr();
        let ctx = LayoutContext {
            region: RegionId::new(31, 32),
            offset: 4096,
            allow_init: true,
        };
        unsafe { <Header<Root> as AdmitLayout>::admit(pointer, (), ctx, None) }.unwrap();
        assert_eq!(unsafe { &*pointer }.layout_id().region, ctx.region);
        assert_eq!(unsafe { &*pointer }.layout_id().offset, ctx.offset);
    }

    #[test]
    fn layout_membership_is_unique_until_the_owner_leaves() {
        let mut storage = MaybeUninit::<Header<Root>>::zeroed();
        let pointer = storage.as_mut_ptr();
        let ctx = LayoutContext {
            region: RegionId::new(33, 34),
            offset: 4096,
            allow_init: true,
        };
        unsafe { <Header<Root> as AdmitLayout>::admit(pointer, (), ctx, Some(3)) }.unwrap();

        assert_eq!(
            unsafe { <Header<Root> as AdmitLayout>::admit(pointer, (), ctx, Some(3)) },
            Err(AdmitError::DuplicateMember)
        );
        assert_eq!(
            unsafe { <Header<Root> as AdmitLayout>::admit(pointer, (), ctx, Some(4)) },
            Ok(())
        );

        unsafe { <Header<Root> as AdmitLayout>::leave(pointer, 3) };
        assert_eq!(
            unsafe { <Header<Root> as AdmitLayout>::admit(pointer, (), ctx, Some(3)) },
            Ok(())
        );
    }

    #[test]
    fn every_recorded_extent_is_checked_before_attachment() {
        static INITS: AtomicUsize = AtomicUsize::new(0);
        let mut storage = MaybeUninit::<Header<Reject>>::zeroed();
        let pointer = storage.as_mut_ptr();
        let ctx = LayoutContext {
            region: RegionId::new(15, 16),
            offset: 0,
            allow_init: true,
        };
        unsafe { <Header<Reject> as AdmitLayout>::admit(pointer, (&INITS, false), ctx, None) }
            .unwrap();
        macro_rules! reject_changed {
            ($member:ident, $field:ident) => {{
                let field = unsafe { ptr::addr_of_mut!((*pointer).$member) };
                let saved = unsafe { field.read() };
                unsafe { field.write(saved.wrapping_add(1)) };
                assert_eq!(
                    unsafe {
                        <Header<Reject> as AdmitLayout>::admit(pointer, (&INITS, false), ctx, None)
                    },
                    Err(AdmitError::Mismatch(LayoutField::$field))
                );
                unsafe { field.write(saved) };
            }};
        }
        reject_changed!(header_size, HeaderSize);
        reject_changed!(header_align, HeaderAlign);
        reject_changed!(info_size, InfoSize);
        reject_changed!(info_align, InfoAlign);
        reject_changed!(body_size, Size);
        reject_changed!(body_align, Align);
    }
}
