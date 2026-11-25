//! Authors each record's words — status, format and profile, magic, schema key and revision, extent
//! and the alignment that extent was made at — plus the member bitset and the root's per-slot member
//! word. A record is written once, by the winner of `admit`, which stores the fields and then
//! publishes; later joiners only set or clear their own bit through `claim` and `leave`. A member word
//! moves only through `Root`: `join` and `reserve` take a free slot, `materialise` and `cancel` end a
//! promise, and `leave`, `mark_dead` and `release_dead` end a membership — each one a
//! compare-exchange on the exact generation it names, so no process moves a word it cannot cite.
//!
//! The non-atomic fields are readable only because that publishing store says they are: `recorded_at`
//! acquire-loads the status and reads the rest plainly, and `dir::scan` hands it a relaxed-loaded
//! offset. Writing a field after the store, or reading it without the status load, fails no test
//! inside one process. A record caught `Initializing` is `Contention` for every joiner afterwards and
//! re-initialising it is refused, so a claim that died mid-flight has to be adjudicated, not retried.
use core::{
    mem,
    ops::Deref,
    ptr,
    sync::atomic::{AtomicU8, AtomicUsize, Ordering},
};

use crate::place::{LayoutContext, LayoutId, RegionId};
use crate::schema::{LayoutInfo, SchemaId, SchemaKey, SharedSchema, schema_id};

pub(crate) type Magic = u16;

const RECORD_FORMAT: u8 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AbiProfile {
    pub word_bytes: u8,
    pub atomic_bytes: u8,
    pub little_endian: bool,
}

impl AbiProfile {
    pub const NATIVE: Self = Self {
        word_bytes: mem::size_of::<usize>() as u8,
        atomic_bytes: mem::size_of::<AtomicUsize>() as u8,
        little_endian: cfg!(target_endian = "little"),
    };
}

#[repr(C)]
pub(crate) struct Record {
    status: AtomicU8,
    format: u8,
    profile: [u8; 3],
    magic: [u8; 2],
    revision: [u8; 4],
    schema: [u8; 8],
    extent: [u8; 8],
    extent_align: [u8; 8],
}

impl Record {
    unsafe fn initialize<L: Layout>(pointer: *mut Self, layout: core::alloc::Layout) {
        unsafe {
            ptr::addr_of_mut!((*pointer).format).write(RECORD_FORMAT);
            ptr::addr_of_mut!((*pointer).profile).write([
                AbiProfile::NATIVE.word_bytes,
                AbiProfile::NATIVE.atomic_bytes,
                AbiProfile::NATIVE.little_endian as u8,
            ]);
            ptr::addr_of_mut!((*pointer).magic).write(L::MAGIC.to_le_bytes());
            ptr::addr_of_mut!((*pointer).revision).write(L::SCHEMA.revision.to_le_bytes());
            ptr::addr_of_mut!((*pointer).schema).write(L::SCHEMA.id.0.to_le_bytes());
            ptr::addr_of_mut!((*pointer).extent).write((layout.size() as u64).to_le_bytes());
            ptr::addr_of_mut!((*pointer).extent_align).write((layout.align() as u64).to_le_bytes());
        }
    }

    fn profile(&self) -> AbiProfile {
        let [word_bytes, atomic_bytes, little_endian] = self.profile;
        AbiProfile {
            word_bytes,
            atomic_bytes,
            little_endian: little_endian != 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RecordedLayout {
    pub(crate) schema: SchemaKey,
    pub(crate) magic: Magic,
    pub(crate) extent: usize,
    pub(crate) extent_align: usize,
}

const _: () = {
    assert!(mem::align_of::<Record>() == 1);
    assert!(mem::size_of::<Record>() == 35);
};

#[cfg(test)]
static CLAIM_TEST_TARGET: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static CLAIM_TEST_WAITERS: AtomicUsize = AtomicUsize::new(0);

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
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
/// `Self` and `Info` are `Sync` because an admitted record is reachable as `&L` from every thread
/// that holds the mapping, not only from the thread that admitted it. A layout with interior
/// mutability must therefore state its own synchronization rule by implementing `Sync`; without the
/// bound, that rule would be a comment on the mapping type while the mutable access came from here.
///
/// # Safety
///
/// Implementors must give the declared schema a stable shared representation.
/// `init` must fully initialize `destination` without first reading it.
pub(crate) unsafe trait Layout: SharedSchema + Sized + Sync {
    type Config: Copy;
    type Info: LayoutInfo + Sync;

    const MAGIC: Magic;

    fn storage(conf: &Self::Config) -> Option<core::alloc::Layout> {
        let _ = conf;
        Some(core::alloc::Layout::new::<RcHeader<Self>>())
    }

    fn info(conf: &Self::Config, ctx: LayoutContext) -> Self::Info;

    /// Initializes a previously uninitialized layout body.
    ///
    /// # Safety
    ///
    /// `destination` must be valid, properly aligned, writable storage for one
    /// `Self`. No live value may currently occupy that storage.
    unsafe fn init(destination: *mut Self, conf: Self::Config) -> Status;
    /// Whether a member may attach to an already-initialized layout body.
    ///
    /// `admit` reaches this only after `storage(conf)` returned a layout and each recorded field
    /// agreed with it, so a verdict re-derived from `conf` alone restates one already given.
    fn attach(&self, conf: &Self::Config) -> Status;

    /// Repairs ownership left by `context.dead()` and returns true only when
    /// no such ownership remains in this layout body.
    fn recover(_: RecoveryContext<'_, Self>) -> bool {
        false
    }
}

pub(crate) trait AdmitLayout: Layout {
    unsafe fn admit(
        pointer: *mut Self,
        conf: Self::Config,
        ctx: LayoutContext,
        slot: Option<u8>,
    ) -> Result<(), RecordAdmitError>;

    unsafe fn leave(pointer: *mut Self, slot: u8);
}

#[repr(C)]
pub(crate) struct RcHeader<T: Layout> {
    record: Record,
    members: AtomicUsize,
    region: RegionId,
    offset: u64,
    info_schema: SchemaId,
    info_revision: u32,
    info: T::Info,
    pub inner: T,
}

/// Which word of a published record refused, and the only cause alphabet a caller can match on.
///
/// Every arm names a *word*, not a reason: the question one answers is "at which byte did the reader
/// have to stop", and the other question — which step of a request declined, and what to do about it —
/// is answered by the layer's own refusal type, not here.
///
/// Three of the words carry more than one question, and that is a limit written down rather than an
/// oversight to fix. `State` collapses the three status values that are not `Initialized` — never
/// written, mid-initialization, and every byte value the decode has to read as corrupted — into one
/// refusal, because each of them ends the read the same way, and it also answers a step that is not
/// permitted to initialize an empty record. `Extent` and `ExtentAlign`
/// each refuse because the word does not fit this host's `usize`, because it is not a legal size or
/// alignment in itself, and because it disagrees with the caller's own type. A caller that had to tell
/// those apart would need a reason type beside the field, and giving one is a public-face change rather
/// than a documentation one; nothing today asks for it, because every leg has the same repair: these
/// bytes and this build do not describe one layout through this entrance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutField {
    /// The status word does not permit this read or admission to go on.
    State,
    /// The record's own framing version is not the one this build reads.
    Format,
    /// The word width, atomic width or byte order recorded is not this build's.
    Profile,
    /// The two-byte tag is not the one this layout writes.
    Magic,
    /// The composed schema id is not this type's.
    Schema,
    /// The schema id matched and the revision did not.
    Revision,
    /// The region the header names is not the one it was read out of.
    Region,
    /// The offset the header names is not where the header was found.
    Offset,
    /// The stored info's schema id is not this build's info type's.
    InfoSchema,
    /// The stored info's schema revision is not this build's info type's.
    InfoRevision,
    /// The extent word — unreadable here, too small to hold a record, or not what this type costs.
    Extent,
    /// The extent alignment word — unreadable here, not a power of two the address satisfies, or not
    /// what this type requires.
    ExtentAlign,
    /// The information bytes are not the ones this type would write for this configuration.
    Info,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecordAdmitError {
    Contention,
    Closed,
    DuplicateMember,
    InvalidHeader,
    Mismatch(LayoutField),
}

pub(crate) struct RecoveryContext<'a, L: Layout> {
    pub layout: &'a L,
    pub info: L::Info,
    pub extent: usize,
    pub dead: u8,
    pub live: u8,
    pub(crate) base: *const u8,
    pub(crate) directory: &'a crate::dir::MapDirectory,
}

type RecoverFn =
    unsafe fn(&crate::dir::MapDirectory, crate::dir::Id<()>, RecordedLayout, u8, u8) -> bool;

/// One layout's standing to settle a dead participant's member of that layout.
///
/// A reap matches a handler to a member by the [`SchemaKey`] the member's own header recorded, never
/// by what the caller expects to find there. An [`AbiProfile`] other than this build's is refused
/// while the header is decoded, before any handler is consulted, so the schema alone decides. A
/// handler that does not match is not run, a member no handler matches stops the reap, and a member
/// two handlers match stops it too. [`crate::layout::recovery_handler`] is the only public
/// entrance: it mints one for a channel member carrying a given payload.
#[derive(Clone, Copy)]
pub struct RecoveryHandler {
    pub(crate) schema: SchemaKey,
    pub(crate) run: RecoverFn,
}

impl RecoveryHandler {
    /// A handler for `L` as this build implements it. A record whose stored ABI profile is not this
    /// build's never reaches the match: `recorded_at` refuses it, so the schema alone decides.
    pub(crate) const fn of<L: Layout>() -> Self {
        Self {
            schema: L::SCHEMA,
            run: recover_as::<L>,
        }
    }
}

unsafe fn recover_as<L: Layout>(
    dir: &crate::dir::MapDirectory,
    id: crate::dir::Id<()>,
    recorded: RecordedLayout,
    dead: u8,
    live: u8,
) -> bool {
    unsafe { dir.recover_layout::<L>(id, recorded, dead, live) }
}

impl RecordedLayout {
    pub(crate) fn mismatch<L: Layout>(self) -> Option<LayoutField> {
        (self.magic != L::MAGIC)
            .then_some(LayoutField::Magic)
            .or_else(|| (self.schema.id != L::SCHEMA.id).then_some(LayoutField::Schema))
            .or_else(|| {
                (self.schema.revision != L::SCHEMA.revision).then_some(LayoutField::Revision)
            })
    }
}

pub(crate) unsafe fn recorded_at(pointer: *const u8) -> Result<RecordedLayout, RecordAdmitError> {
    let record = unsafe { &*pointer.cast::<Record>() };
    if Status::from_u8(record.status.load(Ordering::Acquire)) != Status::Initialized {
        return Err(RecordAdmitError::Mismatch(LayoutField::State));
    }
    if record.format != RECORD_FORMAT {
        return Err(RecordAdmitError::Mismatch(LayoutField::Format));
    }
    if record.profile() != AbiProfile::NATIVE {
        return Err(RecordAdmitError::Mismatch(LayoutField::Profile));
    }
    let extent = usize::try_from(u64::from_le_bytes(record.extent))
        .map_err(|_| RecordAdmitError::Mismatch(LayoutField::Extent))?;
    let extent_align = usize::try_from(u64::from_le_bytes(record.extent_align))
        .map_err(|_| RecordAdmitError::Mismatch(LayoutField::ExtentAlign))?;
    if extent < mem::size_of::<Record>() {
        return Err(RecordAdmitError::Mismatch(LayoutField::Extent));
    }
    if !extent_align.is_power_of_two() || !pointer.addr().is_multiple_of(extent_align) {
        return Err(RecordAdmitError::Mismatch(LayoutField::ExtentAlign));
    }
    Ok(RecordedLayout {
        schema: SchemaKey::new(
            SchemaId(u64::from_le_bytes(record.schema)),
            u32::from_le_bytes(record.revision),
        ),
        magic: u16::from_le_bytes(record.magic),
        extent,
        extent_align,
    })
}

const CLOSED: usize = 1usize << (usize::BITS - 1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MemberCloseError {
    Contention,
    MissingMember,
    Closed,
}

impl<T: Layout + core::fmt::Debug> core::fmt::Debug for RcHeader<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let status = Status::from_u8(self.record.status.load(Ordering::Relaxed));
        let magic = u16::from_le_bytes(self.record.magic);
        f.debug_struct("RcHeader")
            .field("magic", &magic)
            .field("status", &status)
            .field("inner", &self.inner)
            .finish()
    }
}

impl<T: Layout> Deref for RcHeader<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T: Layout> RcHeader<T> {
    #[inline]
    pub fn status(&self) -> Status {
        Status::from_u8(self.record.status.load(Ordering::Acquire))
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

    pub(crate) fn close_unique(&self, slot: u8) -> Result<(), MemberCloseError> {
        let bit = 1usize << slot;
        self.members
            .compare_exchange(bit, CLOSED, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|observed| {
                if observed & CLOSED != 0 {
                    MemberCloseError::Closed
                } else if observed & bit == 0 {
                    MemberCloseError::MissingMember
                } else {
                    MemberCloseError::Contention
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
        unsafe { recorded_at(pointer.cast()) }.ok()?;
        Some(unsafe { ptr::addr_of!((*pointer).region).read() })
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

    /// Wrapping a payload must not invent a second extent: a record's allocation is what
    /// `T::storage` already answers.
    fn storage(conf: &Self::Config) -> Option<core::alloc::Layout> {
        T::storage(conf)
    }

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
    ) -> Result<(), RecordAdmitError> {
        let layout = T::storage(&conf).ok_or(RecordAdmitError::InvalidHeader)?;
        let status = unsafe { &*ptr::addr_of!((*pointer).record.status) };
        loop {
            match Status::from_u8(status.load(Ordering::Acquire)) {
                Status::Uninitialized if !ctx.allow_init => {
                    return Err(RecordAdmitError::Mismatch(LayoutField::State));
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
                Status::Initializing => return Err(RecordAdmitError::Contention),
                Status::Corrupted => return Err(RecordAdmitError::InvalidHeader),
                Status::Initialized => {
                    let recorded = unsafe { recorded_at(pointer.cast()) }?;
                    if let Some(field) =
                        unsafe { Self::mismatch_at(pointer, &conf, layout, recorded, ctx) }
                    {
                        return Err(RecordAdmitError::Mismatch(field));
                    }
                    let members = unsafe { &*ptr::addr_of!((*pointer).members) };
                    let mut membership = Self::claim(members, slot)?;
                    return match T::attach(unsafe { &*ptr::addr_of!((*pointer).inner) }, &conf) {
                        Status::Initialized => {
                            membership.disarm();
                            Ok(())
                        }
                        Status::Initializing => Err(RecordAdmitError::Contention),
                        _ => Err(RecordAdmitError::InvalidHeader),
                    };
                }
            }
        }

        let mut claim = InitClaim::new(status);
        let info = T::info(&conf, ctx);
        let bit = slot.map_or(0, |slot| 1usize << slot);
        unsafe {
            Record::initialize::<T>(ptr::addr_of_mut!((*pointer).record), layout);
            ptr::addr_of_mut!((*pointer).members).write(AtomicUsize::new(bit));
            ptr::addr_of_mut!((*pointer).region).write(ctx.region);
            ptr::addr_of_mut!((*pointer).offset).write(ctx.offset);
            ptr::addr_of_mut!((*pointer).info_schema).write(T::Info::SCHEMA.id);
            ptr::addr_of_mut!((*pointer).info_revision).write(T::Info::SCHEMA.revision);
            ptr::addr_of_mut!((*pointer).info).write(info);
        }
        let members = unsafe { &*ptr::addr_of!((*pointer).members) };
        let mut membership = Membership::new(members, bit);
        if unsafe { T::init(ptr::addr_of_mut!((*pointer).inner), conf) } != Status::Initialized {
            return Err(RecordAdmitError::InvalidHeader);
        }
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
    /// Seat a member in the word `members` names, or refuse with the cause that says why.
    ///
    /// The caller reaches the word, so the refusals here are decisions about its bits — closed,
    /// already seated, or contested — and the guard carries the same borrow back out.
    fn claim<'a>(
        members: &'a AtomicUsize,
        slot: Option<u8>,
    ) -> Result<Membership<'a>, RecordAdmitError> {
        let Some(slot) = slot else {
            return Ok(Membership::new(members, 0));
        };
        let bit = 1usize << slot;
        let observed = members.load(Ordering::Acquire);
        if observed & CLOSED != 0 {
            return Err(RecordAdmitError::Closed);
        }
        if observed & bit != 0 {
            return Err(RecordAdmitError::DuplicateMember);
        }
        members
            .compare_exchange(
                observed,
                observed | bit,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| Membership::new(members, bit))
            .map_err(|_| RecordAdmitError::Contention)
    }
}

impl<T: Layout> RcHeader<T> {
    pub(crate) unsafe fn inspect_at(
        pointer: *const Self,
        ctx: LayoutContext,
    ) -> Result<T::Info, RecordAdmitError> {
        let recorded = unsafe { recorded_at(pointer.cast()) }?;
        unsafe { Self::inspect_recorded(pointer, ctx, recorded) }
            .map_err(RecordAdmitError::Mismatch)
    }

    /// Verify a header against a record its reader already decoded at this same address.
    ///
    /// # Safety
    ///
    /// `recorded` must be the decode of `pointer` — what `recorded_at(pointer.cast())` answers. The
    /// identity and extent legs of a record are written once, by `Record::initialize`, and never
    /// rewritten, so a decode taken while the status reads `Initialized` stays that record's answer;
    /// a decode from a different address would let this frame certify bytes it never read.
    pub(crate) unsafe fn inspect_recorded(
        pointer: *const Self,
        ctx: LayoutContext,
        recorded: RecordedLayout,
    ) -> Result<T::Info, LayoutField> {
        if let Some(field) = recorded.mismatch::<T>() {
            return Err(field);
        }
        macro_rules! check {
            ($member:ident, $expected:expr, $field:ident) => {
                if unsafe { ptr::addr_of!((*pointer).$member).read() } != $expected {
                    return Err(LayoutField::$field);
                }
            };
        }
        check!(region, ctx.region, Region);
        check!(offset, ctx.offset, Offset);
        check!(info_schema, T::Info::SCHEMA.id, InfoSchema);
        check!(info_revision, T::Info::SCHEMA.revision, InfoRevision);
        Ok(unsafe { ptr::addr_of!((*pointer).info).read() })
    }

    /// The reader's seven legs plus the two a reader cannot answer for itself: what the record claims
    /// to hold, and what the caller's own type says a value of it costs. Those two are why this is the
    /// writer's entrance and not a shared verifier — they consult a `Config`.
    ///
    /// The extent is compared as the decoded number, not as the stored bytes: `recorded_at` already
    /// refused whatever it cannot represent, so re-encoding `layout` and reading `[u8; 8]` again would
    /// put the same question to a second spelling of the same quantity.
    unsafe fn mismatch_at(
        pointer: *const Self,
        conf: &T::Config,
        layout: core::alloc::Layout,
        recorded: RecordedLayout,
        ctx: LayoutContext,
    ) -> Option<LayoutField> {
        let info = match unsafe { Self::inspect_recorded(pointer, ctx, recorded) } {
            Ok(info) => info,
            Err(field) => return Some(field),
        };
        if recorded.extent != layout.size() {
            return Some(LayoutField::Extent);
        }
        if recorded.extent_align != layout.align() {
            return Some(LayoutField::ExtentAlign);
        }
        if info != T::info(conf, ctx) {
            return Some(LayoutField::Info);
        }
        None
    }
}

/// A member bit this process holds in one header's member word, discharged when the guard goes
/// away. It borrows the word rather than recording its address, which is what [`InitClaim`] does
/// for the status byte beside it: the clear in `Drop` is reached through the same borrow the claim
/// was made against, so a guard cannot outlive the header it seated into and restates a bit in
/// bytes no other path is naming.
struct Membership<'a> {
    members: &'a AtomicUsize,
    bit: usize,
}

impl<'a> Membership<'a> {
    const fn new(members: &'a AtomicUsize, bit: usize) -> Self {
        Self { members, bit }
    }

    const fn disarm(&mut self) {
        self.bit = 0;
    }
}

impl Drop for Membership<'_> {
    fn drop(&mut self) {
        if self.bit != 0 {
            self.members.fetch_and(!self.bit, Ordering::AcqRel);
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
    ) -> Result<(), RecordAdmitError> {
        Ok(())
    }

    unsafe fn leave(_: *mut Self, _: u8) {}
}

pub(crate) const PARTICIPANT_CAPACITY: usize = usize::BITS as usize - 1;

/// How a root's member word is cut, published because a process joining a region without this crate
/// has to write that word the same way. The low `MEMBER_STATE_BITS` hold one of the five states
/// below and the rest hold the member's generation, so a word is
/// `(generation << MEMBER_STATE_BITS) | state` and the two halves separate with
/// `MEMBER_STATE_MASK`. This is the same kind of fact as
/// [`crate::mapping::REGION_ALIGN`], published for the same reason.
///
/// The width is a `usize` because one member is one atomic, which makes the generation's ceiling
/// target-dependent: [`MEMBER_GENERATION_MAX`] is where a claim is refused rather than wrapped.
/// Five states in three bits leaves 5, 6 and 7 unassigned, and the freeze below is what makes
/// taking one of them a deliberate act — a further state has to be said so here before the word
/// will hold it. `tests/portable_contracts/member_word.rs` pins every number here as published.
pub const MEMBER_STATE_BITS: usize = 3;
/// The mask over the state half of a member word.
pub const MEMBER_STATE_MASK: usize = (1 << MEMBER_STATE_BITS) - 1;
/// The largest generation a member word holds.
pub const MEMBER_GENERATION_MAX: usize = usize::MAX >> MEMBER_STATE_BITS;
/// No process holds the member: what a fresh word holds, and what a released seat or a promise no
/// arrival filled returns to.
pub const MEMBER_FREE: usize = 0;
/// The member's process stands.
pub const MEMBER_LIVE: usize = 1;
/// That process is gone, in the generation the word still names.
pub const MEMBER_DEAD: usize = 2;
/// A reap settled the member and kept its generation, so evidence older than the reap still
/// resolves to the member it named.
pub const MEMBER_RETIRED: usize = 3;
/// A parent promised this member a seat, and no arrival has filled the promise yet.
pub const MEMBER_RESERVED: usize = 4;

// Five states in three bits, stated once more as a compile-time fact than the docs above claim: 5, 6
// and 7 stay unused until a line here says otherwise.
const _: () = assert!(MEMBER_RESERVED == 4 && MEMBER_RESERVED + 1 < 1 << MEMBER_STATE_BITS);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Member {
    pub(crate) slot: u8,
    pub(crate) generation: usize,
}

/// The result of trying to take a member slot, which says more than whether one was had.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Claim {
    Taken(Member),
    /// Nothing was free, but a slot stands promised to a process that has not arrived. Cancelling
    /// that promise makes room, so this is a refusal a caller may retry rather than a capacity limit.
    Promised,
    /// Every slot is held by a member that is live, dead or retired.
    Full,
}

/// The region's roster: one word per participant slot, `(generation << 3) | state`.
///
/// A slot is claimed at map time by [`Root::join`] and released by [`Root::leave`], and a generation
/// is spent by whichever of the two claims it, so a word never repeats an identity that was already
/// answerable for. `RESERVED` is the one state a process does not write about itself: a holder-to-be
/// is named there by somebody else ([`Root::reserve`]) and taken up by the process it names
/// ([`Root::materialise`]), which is what leaves the reserver something to cancel
/// ([`Root::cancel`]) if that process never arrives.
///
/// # Ordering
///
/// Every transition is a compare-exchange on the exact word it expects, so no state is reached from
/// a neighbour it is not listed under: only `FREE` becomes `LIVE` or `RESERVED`, only `LIVE` becomes
/// `DEAD` or `FREE`, and a `RESERVED` slot is invisible to the death machinery, which is the point —
/// nobody has died if nobody arrived.
#[repr(C)]
#[derive(Debug)]
pub(crate) struct Root {
    members: [AtomicUsize; PARTICIPANT_CAPACITY],
}

pub(crate) type RootHeader = RcHeader<Root>;

impl SharedSchema for Root {
    /// Revision 2 widened the member state field to make room for `RESERVED`, which re-fits the
    /// generation beside it; a region rostered under revision 1 is refused rather than reinterpreted.
    const SCHEMA: SchemaKey = SchemaKey::new(schema_id("evering.root"), 2);
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
    pub(crate) fn join(&self) -> Claim {
        self.claim(MEMBER_LIVE)
    }

    /// Name a holder for a slot that nobody has taken yet, at the generation they will arrive at.
    pub(crate) fn reserve(&self) -> Claim {
        self.claim(MEMBER_RESERVED)
    }

    /// Take the lowest unheld slot and park it in `state`, spending the generation its next holder
    /// answers to.
    ///
    /// A slot at the last generation is retired on the way past rather than claimed, because the
    /// next generation would not fit in the word; both paths need that, so neither scans on its own.
    fn claim(&self, state: usize) -> Claim {
        let mut promised = false;
        for (slot, cell) in self.members.iter().enumerate() {
            let observed = cell.load(Ordering::Acquire);
            if observed & MEMBER_STATE_MASK != MEMBER_FREE {
                promised |= observed & MEMBER_STATE_MASK == MEMBER_RESERVED;
                continue;
            }
            let prior = observed >> MEMBER_STATE_BITS;
            if prior == MEMBER_GENERATION_MAX {
                let _ = cell.compare_exchange(
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
            let next = (generation << MEMBER_STATE_BITS) | state;
            if cell
                .compare_exchange(observed, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Claim::Taken(Member {
                    slot: slot as u8,
                    generation,
                });
            }
        }
        if promised {
            Claim::Promised
        } else {
            Claim::Full
        }
    }

    fn word(member: Member, state: usize) -> usize {
        (member.generation << MEMBER_STATE_BITS) | state
    }

    fn member(&self, member: Member) -> Option<&AtomicUsize> {
        self.members.get(member.slot as usize)
    }

    pub(crate) fn leave(&self, member: Member) -> bool {
        self.transition(member, MEMBER_LIVE, MEMBER_FREE)
    }

    /// Arrive in the slot that was reserved for this process, at the generation it was reserved at.
    ///
    /// A word that no longer reads `RESERVED(G)` is owned by somebody else now, so an arrival cannot
    /// displace a live member, a slot that was given back, or one that was never promised.
    pub(crate) fn materialise(&self, member: Member) -> bool {
        self.transition(member, MEMBER_RESERVED, MEMBER_LIVE)
    }

    /// Give back a slot nobody arrived to take. The generation it burned stays spent, and a slot
    /// somebody else took up or holds is not this one's to return.
    pub(crate) fn cancel(&self, member: Member) -> bool {
        self.transition(member, MEMBER_RESERVED, MEMBER_FREE)
    }

    /// Move `member`'s word from one state to the next, which is what makes every transition in this
    /// roster a compare-exchange on the exact generation it names.
    fn transition(&self, member: Member, from: usize, to: usize) -> bool {
        let Some(state) = self.member(member) else {
            return false;
        };
        state
            .compare_exchange(
                Self::word(member, from),
                Self::word(member, to),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(crate) fn mark_dead(&self, member: Member) -> bool {
        self.transition(member, MEMBER_LIVE, MEMBER_DEAD)
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
    use super::{
        AdmitLayout, Layout, LayoutField, RECORD_FORMAT, RcHeader as Header, RecordAdmitError,
        Root, Status,
    };
    use crate::place::{LayoutContext, RegionId};
    use crate::schema::{SchemaKey, SharedSchema, compose_const, schema_id};
    use core::mem::MaybeUninit;
    use core::panic::AssertUnwindSafe;
    use core::sync::atomic::{AtomicUsize, Ordering};

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
            Err(RecordAdmitError::InvalidHeader)
        );
        assert_eq!(
            unsafe { <Header<Reject> as AdmitLayout>::admit(header, (&INITS, true), ctx, Some(3)) },
            Err(RecordAdmitError::InvalidHeader)
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
        let conf = &(&INITS, false);
        let recorded = unsafe { super::recorded_at(header as *const u8) }.unwrap();
        assert_eq!(
            unsafe {
                Header::<Reject>::mismatch_at(
                    header,
                    conf,
                    <Reject as Layout>::storage(conf).unwrap(),
                    recorded,
                    moved,
                )
            },
            Some(LayoutField::Offset)
        );
        assert_eq!(
            unsafe { <Header<Reject> as AdmitLayout>::admit(header, (&INITS, false), moved, None) },
            Err(RecordAdmitError::Mismatch(LayoutField::Offset))
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
            Err(RecordAdmitError::InvalidHeader)
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
        let first = taken(unsafe { (*pointer).inner.join() });
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
            Err(RecordAdmitError::Mismatch(LayoutField::Region))
        );
        assert!(unsafe { (*pointer).inner.leave(first) });
    }

    #[test]
    fn a_wider_or_less_aligned_record_is_refused_by_the_leg_that_changed() {
        // Each of the two extent legs has to be able to fire on its own, or the comparison it
        // belongs to is not answering a question that anything else answers.
        let mut storage = MaybeUninit::<Header<Root>>::zeroed();
        let header = storage.as_mut_ptr();
        let ctx = LayoutContext {
            region: RegionId::new(51, 52),
            offset: 0,
            allow_init: true,
        };
        assert_eq!(
            unsafe { <Header<Root> as AdmitLayout>::admit(header, (), ctx, None) },
            Ok(())
        );
        let reattach = LayoutContext {
            allow_init: false,
            ..ctx
        };
        let recorded = unsafe { super::recorded_at(header as *const u8) }.unwrap();
        let stored = <Root as Layout>::storage(&()).unwrap();
        let wider =
            core::alloc::Layout::from_size_align(stored.size() + stored.align(), stored.align())
                .unwrap();
        assert_eq!(
            unsafe { Header::<Root>::mismatch_at(header, &(), wider, recorded, reattach) },
            Some(LayoutField::Extent)
        );
        let remounted =
            core::alloc::Layout::from_size_align(stored.size(), stored.align() * 2).unwrap();
        assert_eq!(
            unsafe { Header::<Root>::mismatch_at(header, &(), remounted, recorded, reattach) },
            Some(LayoutField::ExtentAlign)
        );
    }

    #[test]
    fn a_root_rostered_under_the_previous_encoding_is_refused_by_name() {
        // Revision 2 widened the member state field, which re-fits the generation beside it: a roster
        // written under revision 1 would otherwise be read with the wrong shifts and report somebody
        // else's identities as free slots.
        assert_eq!(Root::SCHEMA.revision, 2);
        let mut storage = MaybeUninit::<Header<Root>>::zeroed();
        let header = storage.as_mut_ptr();
        let ctx = LayoutContext {
            region: RegionId::new(41, 42),
            offset: 0,
            allow_init: true,
        };
        assert_eq!(
            unsafe { <Header<Root> as AdmitLayout>::admit(header, (), ctx, None) },
            Ok(())
        );
        let reattach = LayoutContext {
            allow_init: false,
            ..ctx
        };
        unsafe { core::ptr::addr_of_mut!((*header).record.revision).write(1u32.to_le_bytes()) };
        let revision_one = unsafe { super::recorded_at(header as *const u8) }.unwrap();
        assert_eq!(
            unsafe {
                Header::<Root>::mismatch_at(
                    header,
                    &(),
                    <Root as Layout>::storage(&()).unwrap(),
                    revision_one,
                    reattach,
                )
            },
            Some(LayoutField::Revision)
        );
        assert_eq!(
            unsafe { <Header<Root> as AdmitLayout>::admit(header, (), reattach, None) },
            Err(RecordAdmitError::Mismatch(LayoutField::Revision))
        );
        unsafe { core::ptr::addr_of_mut!((*header).record.revision).write(2u32.to_le_bytes()) };
        assert_eq!(
            unsafe { <Header<Root> as AdmitLayout>::admit(header, (), reattach, None) },
            Ok(())
        );
    }

    #[test]
    fn participant_generation_blocks_stale_owner_and_dead_reuse() {
        let root = roster();
        let first = taken(root.join());
        let others = (1..super::PARTICIPANT_CAPACITY)
            .map(|_| taken(root.join()))
            .collect::<Vec<_>>();
        assert_eq!(root.join(), super::Claim::Full);

        assert!(root.mark_dead(first));
        assert!(root.is_dead(first));
        assert!(!root.leave(first));
        assert_eq!(root.join(), super::Claim::Full);
        assert!(root.release_dead(first));

        let replacement = taken(root.join());
        assert_eq!(replacement.slot, first.slot);
        assert_ne!(replacement.generation, first.generation);
        assert!(!root.leave(first));
        assert!(root.leave(replacement));
        for member in others {
            assert!(root.leave(member));
        }
    }

    fn taken(claim: super::Claim) -> super::Member {
        match claim {
            super::Claim::Taken(member) => member,
            super::Claim::Promised => panic!("the roster promised a slot nobody took"),
            super::Claim::Full => panic!("the roster held no free slot"),
        }
    }

    fn roster() -> Root {
        Root {
            members: [const { AtomicUsize::new(0) }; super::PARTICIPANT_CAPACITY],
        }
    }

    #[test]
    fn a_reservation_holds_its_slot_against_every_other_claim() {
        let root = roster();
        let reserved = taken(root.reserve());
        assert_eq!(reserved.slot, 0);
        assert_eq!(reserved.generation, 1);

        // A promise keeps the slot out of circulation: the next join is handed the one after it.
        let other = taken(root.join());
        assert_eq!(other.slot, 1);
        assert_eq!(
            root.reserve(),
            super::Claim::Taken(super::Member {
                slot: 2,
                generation: 1
            })
        );

        // Arrival happens at exactly the promised generation, once.
        for generation in [reserved.generation + 1, reserved.generation - 1] {
            assert!(!root.materialise(super::Member {
                slot: reserved.slot,
                generation
            }));
        }
        assert!(root.materialise(reserved));
        assert!(!root.materialise(reserved));
        assert!(!root.cancel(reserved));

        // Only after arrival is the slot somebody's, and so somebody's to reap.
        assert!(root.mark_dead(reserved));
        assert!(root.release_dead(reserved));
        let again = taken(root.join());
        assert_eq!(again.slot, reserved.slot);
        assert_eq!(again.generation, reserved.generation + 1);
    }

    #[test]
    fn a_reserved_slot_cannot_die_be_left_or_reaped_because_nobody_holds_it() {
        let root = roster();
        let reserved = taken(root.reserve());
        assert!(!root.mark_dead(reserved));
        assert!(!root.is_dead(reserved));
        assert!(!root.release_dead(reserved));
        assert!(!root.leave(reserved));
        // None of that touched the promise, which is still the reserver's to give back.
        assert!(root.cancel(reserved));
    }

    #[test]
    fn a_cancelled_reservation_frees_its_slot_and_keeps_the_generation_spent() {
        let root = roster();
        let reserved = taken(root.reserve());
        assert!(root.cancel(reserved));
        assert!(!root.cancel(reserved));
        assert!(!root.materialise(reserved));

        let arrived = taken(root.join());
        assert_eq!(arrived.slot, reserved.slot);
        assert_eq!(arrived.generation, reserved.generation + 1);
        // A promise that was given back cannot be cancelled into somebody else's membership.
        assert!(!root.cancel(reserved));
        assert!(!root.cancel(arrived));
        assert!(root.leave(arrived));

        let second = taken(root.reserve());
        assert_eq!(second.generation, arrived.generation + 1);
    }

    #[test]
    fn a_roster_of_promises_refuses_a_join_for_a_different_reason_than_capacity() {
        let root = roster();
        for _ in 0..super::PARTICIPANT_CAPACITY {
            assert!(matches!(root.reserve(), super::Claim::Taken(_)));
        }
        assert_eq!(root.join(), super::Claim::Promised);
        assert_eq!(root.reserve(), super::Claim::Promised);

        // A live member on its own is the capacity answer, because nothing of it is coming back.
        let idle = roster();
        let member = taken(idle.join());
        for _ in 1..super::PARTICIPANT_CAPACITY {
            assert!(matches!(idle.join(), super::Claim::Taken(_)));
        }
        assert_eq!(idle.join(), super::Claim::Full);
        assert!(idle.mark_dead(member));
        assert_eq!(idle.join(), super::Claim::Full);
        assert!(idle.release_dead(member));
        assert_eq!(taken(idle.join()).slot, member.slot);
    }

    #[test]
    fn the_last_generation_is_retired_rather_than_claimed_by_either_path() {
        let exhausted = super::Member {
            slot: 0,
            generation: super::MEMBER_GENERATION_MAX,
        };
        for reserve in [false, true] {
            let root = roster();
            root.members[0].store(Root::word(exhausted, super::MEMBER_FREE), Ordering::Relaxed);
            let parked = if reserve { root.reserve() } else { root.join() };
            let slot = match parked {
                super::Claim::Taken(member) => member.slot,
                other => panic!("a roster with free slots refused one: {other:?}"),
            };
            assert_ne!(slot, 0);
            assert_eq!(
                root.members[0].load(Ordering::Acquire),
                Root::word(exhausted, super::MEMBER_RETIRED),
                "the exhausted slot is retired by the scan that passed it"
            );
            assert_eq!(
                root.members[slot as usize].load(Ordering::Acquire) & super::MEMBER_STATE_MASK,
                if reserve {
                    super::MEMBER_RESERVED
                } else {
                    super::MEMBER_LIVE
                }
            );
        }
    }

    fn race_root(second_region: RegionId) -> [Result<(), RecordAdmitError>; 2] {
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
                .filter(|result| matches!(
                    result,
                    Err(RecordAdmitError::Mismatch(LayoutField::Region))
                ))
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

    unsafe impl crate::schema::LayoutInfo for WideInfo {}

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
            Err(RecordAdmitError::Mismatch(LayoutField::InfoSchema))
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
    fn malformed_bootstrap_profile_is_rejected_before_typed_information() {
        let mut storage = MaybeUninit::<Header<Root>>::zeroed();
        let pointer = storage.as_mut_ptr();
        let ctx = LayoutContext {
            region: RegionId::new(41, 42),
            offset: 0,
            allow_init: true,
        };
        unsafe { <Header<Root> as AdmitLayout>::admit(pointer, (), ctx, None) }.unwrap();
        unsafe { pointer.cast::<u8>().add(1).write(0) };
        assert_eq!(
            unsafe { <Header<Root> as AdmitLayout>::admit(pointer, (), ctx, None) },
            Err(RecordAdmitError::Mismatch(LayoutField::Format))
        );
        unsafe { pointer.cast::<u8>().add(1).write(RECORD_FORMAT) };
        unsafe { pointer.cast::<u8>().add(2).write(0) };
        assert_eq!(
            unsafe { <Header<Root> as AdmitLayout>::admit(pointer, (), ctx, None) },
            Err(RecordAdmitError::Mismatch(LayoutField::Profile))
        );
    }

    #[test]
    fn a_plausible_foreign_profile_is_refused_before_the_record_s_other_words() {
        let mut storage = MaybeUninit::<Header<Narrow>>::zeroed();
        let pointer = storage.as_mut_ptr();
        let ctx = LayoutContext {
            region: RegionId::new(61, 62),
            offset: 0,
            allow_init: true,
        };
        unsafe { <Header<Narrow> as AdmitLayout>::admit(pointer, (), ctx, None) }.unwrap();
        let join = LayoutContext {
            allow_init: false,
            ..ctx
        };
        let other_word = if core::mem::size_of::<usize>() == 8 {
            4
        } else {
            8
        };
        // The control: while the profile stays native, a wide body over a narrow region is refused
        // for the typed word. The same refusal has to become a profile refusal once the record says
        // it was written by a target of the other width, because the typed word is then unreadable.
        assert_eq!(
            unsafe { <Header<Wide> as AdmitLayout>::admit(pointer.cast(), (), join, None) },
            Err(RecordAdmitError::Mismatch(LayoutField::InfoSchema))
        );
        let native = unsafe { (*pointer).record.profile };
        unsafe { (*pointer).record.profile = [other_word, other_word, native[2]] };
        assert_eq!(
            unsafe { <Header<Wide> as AdmitLayout>::admit(pointer.cast(), (), join, None) },
            Err(RecordAdmitError::Mismatch(LayoutField::Profile))
        );
        unsafe { (*pointer).record.profile = native };
        unsafe { (*pointer).record.profile[2] = native[2] ^ 1 };
        assert_eq!(
            unsafe { <Header<Narrow> as AdmitLayout>::admit(pointer, (), join, None) },
            Err(RecordAdmitError::Mismatch(LayoutField::Profile))
        );
    }

    #[test]
    fn a_reader_that_may_not_write_refuses_a_region_of_another_width() {
        let mut storage = MaybeUninit::<Header<Root>>::zeroed();
        let pointer = storage.as_mut_ptr();
        let ctx = LayoutContext {
            region: RegionId::new(63, 64),
            offset: 0,
            allow_init: true,
        };
        unsafe { <Header<Root> as AdmitLayout>::admit(pointer, (), ctx, None) }.unwrap();
        assert_eq!(
            unsafe { Header::<Root>::published_region_at(pointer) },
            Some(ctx.region)
        );
        let native = unsafe { (*pointer).record.profile };
        let other_word = if core::mem::size_of::<usize>() == 8 {
            4
        } else {
            8
        };
        unsafe { (*pointer).record.profile = [other_word, native[1], native[2]] };
        assert_eq!(
            unsafe { Header::<Root>::published_region_at(pointer) },
            None,
            "a discovery reader named a region whose recorded width it could not read"
        );
        unsafe { (*pointer).record.profile = native };
        assert_eq!(
            unsafe { Header::<Root>::published_region_at(pointer) },
            Some(ctx.region)
        );
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
            Err(RecordAdmitError::DuplicateMember)
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
        macro_rules! reject_record {
            ($member:ident, $field:ident) => {{
                unsafe { (*pointer).record.$member[0] ^= 1 };
                assert_eq!(
                    unsafe {
                        <Header<Reject> as AdmitLayout>::admit(pointer, (&INITS, false), ctx, None)
                    },
                    Err(RecordAdmitError::Mismatch(LayoutField::$field))
                );
                unsafe { (*pointer).record.$member[0] ^= 1 };
            }};
        }
        reject_record!(extent, Extent);
        reject_record!(extent_align, ExtentAlign);
        macro_rules! reject_field {
            ($member:ident, $field:ident) => {{
                unsafe { (*pointer).$member += 1 };
                assert_eq!(
                    unsafe {
                        <Header<Reject> as AdmitLayout>::admit(pointer, (&INITS, false), ctx, None)
                    },
                    Err(RecordAdmitError::Mismatch(LayoutField::$field))
                );
                unsafe { (*pointer).$member -= 1 };
            }};
        }
        // `Region` and `Offset` are refused from a peer's own coordinates elsewhere, and `InfoSchema`
        // is what the `Narrow`/`Wide` pair turns on; the revision beside it had no other witness.
        reject_field!(info_revision, InfoRevision);
    }
}
