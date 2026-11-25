//! Owns the linear account of one process-local mapping: the cursor that turns a sequence of composed
//! layouts into addresses, and the arithmetic that reserves a span inside it. `Build`'s `push` and
//! `reserve`, committed through `Reservation::commit`, extend the composition; `Map`'s `reserve` and
//! `commit` do the address arithmetic and the compare-exchange that keeps it honest; `Map::close` hands
//! the account back once, if the extent has an account to hand back — one declared with
//! `Map::from_static` is simply let go. `Region::new` is the one place a mapping names the member it
//! owns, and it takes that word one of two ways: `join` claims whatever slot is next, `materialise`
//! arrives in a slot a live member promised. `Observer` is the same extent read without that word: it
//! holds a mapping, answers what a published root claims, and reaches nothing that reserves, commits or
//! claims — so the account below is one it can read and never change.
//!
//! The cursor advances by `size_of`, never by the extent a configuration says a type needs, so a
//! record whose extent grows with its contents has to be composed last. Nothing asserts that, and a
//! region composed the other way round still opens. The residue a composition records is likewise a
//! promise about the mapping underneath it: `REGION_ALIGN` says what the promise is, so a port can meet
//! it on purpose, and a `Source` written outside this crate can still hand back a `Map` that misses it —
//! which a peer finds out at its own admission, not here.
use alloc::sync::Arc;
use core::ptr::NonNull;
use core::{marker::PhantomData, ops::Deref};

use crate::{
    header::{AdmitLayout, Claim, Layout, Member, RecordAdmitError, RootHeader},
    place::{LayoutContext, RegionAdmission, RegionId},
};

pub use crate::header::LayoutField;
#[cfg(test)]
pub use alloc::alloc::AllocError;

bitflags::bitflags! {
    /// What a mapping may do to the bytes, and the only access language a port has to answer in.
    ///
    /// The three bits mean different things on the two hosts, and a caller that reads one arm's
    /// behaviour into the other gets a refusal on one and silence on the other. On Unix they become the
    /// protection of *one mapping*, so each process decides for itself and the object underneath
    /// restricts nobody — except that an object made through `os::shared` keeps the set it was made
    /// under as its own bound, which this crate then refuses to widen. On Windows they select the
    /// protection a section is *created* with, which limits every process that maps it, and a view can
    /// only narrow from it.
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Access: u8 {
        /// Read the bytes. The Windows arm requires it twice — of the section as it is created and of
        /// the view as it is opened — and refuses the call itself when it is absent, while the Unix arm
        /// asks for nothing and passes the request straight to `mmap` as protection bits.
        const READ = 0x1;
        /// Write the bytes, which is what admitting a layout to a region requires — every composition
        /// in this crate asks for it, because the header and the directory are written, not read.
        const WRITE = 0x1 << 1;
        /// Execute the bytes. Nothing in this crate asks for it: it exists because a port's host may
        /// grant it and a caller has to be able to say so, and on Windows it changes the protection the
        /// section is created under rather than only the view.
        const EXEC = 0x1 << 2;
    }
}

impl core::fmt::Display for Access {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self, f)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    pub len: usize,
    pub access: Access,
}

impl Request {
    pub const fn new(len: usize, access: Access) -> Self {
        Self { len, access }
    }
}

/// Produces one exclusively owned process-local mapping.
///
/// The `os` adapters report `io::Error` for every platform's mapping failure, so
/// `SessionError<S::Error>` stays one type across them. A target with no mapping
/// call at all — a region fixed in the program's own image — implements this trait
/// by returning a [`Map::from_static`], and is the only kind of implementation the
/// safety note below does not constrain away.
///
/// # Safety
///
/// An implementation must return a valid mapping with the requested access and extent, based at an
/// address that is a multiple of [`REGION_ALIGN`]. That bound is what makes a block address
/// reproducible in a peer: a pool caps its widest class at it, and `header::recorded_at` refuses a
/// member whose own base is not a multiple of the extent alignment the region recorded. A release
/// function must remain callable and release that mapping exactly once from any thread that may own
/// it; an extent with nothing to release is declared with [`Map::from_static`] rather than by a release
/// that lies.
pub unsafe trait Source: Sized {
    type Error: core::fmt::Debug;

    fn map(self, request: Request) -> Result<Map, Self::Error>;
}

pub enum Error {
    PermissionDenied {
        requested: Access,
    },
    UnenoughSpace {
        requested: usize,
        allocated: usize,
    },
    Contention,
    LayoutClosed,
    DuplicateAttachment,
    InvalidHeader,
    LayoutMismatch(LayoutField),
    PoisonedComposition,
    ArithmeticOverflow,
    /// No member slot could be given, and `waits` says whose act would free one.
    NoRoom {
        waits: RoomWait,
    },
    ArrivalRefused,
}

impl core::error::Error for Error {}

impl core::fmt::Debug for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::PermissionDenied { requested } => {
                write!(f, "Permission denied, requested {requested:?}")
            }
            Self::UnenoughSpace {
                requested,
                allocated,
            } => write!(
                f,
                "Not enough space available, requested {requested}, allocated {allocated}"
            ),
            Self::Contention => f.write_str("Contention"),
            Self::LayoutClosed => f.write_str("Shared layout is closed"),
            Self::DuplicateAttachment => {
                f.write_str("This participant already attached the shared layout")
            }
            Self::InvalidHeader => f.write_str("Header initialization failed"),
            Self::LayoutMismatch(field) => write!(f, "Layout mismatch: {field:?}"),
            Self::PoisonedComposition => f.write_str("Layout composition is poisoned"),
            Self::ArithmeticOverflow => f.write_str("Layout cursor arithmetic overflow"),
            Self::NoRoom { waits } => {
                write!(f, "No shared-memory participant slot is free, so {waits}")
            }
            Self::ArrivalRefused => f.write_str(
                "The shared-memory participant slot named for arrival is not promised at that \
                 generation",
            ),
        }
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self, f)
    }
}

/// Whose act would change a `NoRoom` answer.
///
/// The four are genuinely different situations, and what makes them different is not the fact — the
/// fact is always "nothing was free" — but which act elsewhere makes it false. Naming the act is why
/// they share one arm with a field instead of four arms: a caller's wait policy follows `waits`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoomWait {
    /// Something claimed has to be released back into this region.
    Release,
    /// The consuming end has to take what is already staged.
    Drain,
    /// The region itself has to grow before the request fits.
    Grow,
    /// A member slot promised to a process that has not arrived has to be filled or freed.
    Promise,
}

/// The act, in one clause, that would make a `NoRoom` answer false. This is the only place those four
/// sentences are written: a declined request ([`DeclineCause::NoRoom`]) and an entrance that could not
/// promise a slot ([`Error::NoRoom`], crate::session's `SeatError` and `SessionError`) answer the same
/// question, and spelling the act a second time is how the two would drift apart.
impl core::fmt::Display for RoomWait {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Release => f.write_str("a release elsewhere frees one"),
            Self::Drain => f.write_str("a receiver has to claim what is already staged"),
            Self::Grow => f.write_str("the region has to grow before it fits"),
            Self::Promise => {
                f.write_str("a member promised and not yet arrived has to be filled or dropped")
            }
        }
    }
}

/// Why an authority said no to a request, apart from whatever it is holding for the caller.
///
/// Every arm answers the same question — what would have to change for this request to succeed — and
/// none of them is a condition of the caller's value, which is why the value is a companion in
/// [`Decline`] rather than a field of one arm here. Entry refusals (wrong region, no such record,
/// field disagreement) answer a different question and keep their own types; this is the table a
/// *request* is declined with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeclineCause {
    /// The request is wider than anything this authority can hold, so only a smaller request answers.
    TooWide,
    /// Nothing was free where the request needed room, and `waits` says whose act would free it.
    NoRoom { waits: RoomWait },
    /// Another member's authority word covered the slot this request needed.
    ///
    /// `member` names that participant slot where the word still says who held it, and `None` where
    /// the race left no name behind — a lost cursor compare-exchange knows it lost and not to whom. A
    /// slot rather than a [`Peer`](crate::mapping::Peer), because a compare-exchange loses to a word
    /// whose generation nobody has read, and inventing one would hand out a credential for nothing.
    Busy { member: Option<u8> },
    /// The layout is closed, so this kind of request will not be answered again.
    Closed,
    /// An authority word names a state no participant writes: the region needs adjudication, not a retry.
    Corrupted,
    /// A mutation word is poisoned — what a writer that died mid-change leaves, and what `Recovery` settles.
    Poisoned,
    /// The request's own layout arithmetic overflowed, so no extent of this region could hold it.
    Overflow,
    /// This mapping cannot be written, so no member could be admitted to the header the request needed
    /// to write in region `region`.
    ///
    /// This is the arm that used to be dropped on the floor: the inner answer already knew the mapping
    /// was read-only while the outer sentence said "no room" or "storage failed". It is why the cause
    /// chain is wired once — an outer type that would otherwise flatten a refusal carries a
    /// `DeclineCause` and forwards [`source`](core::error::Error::source) to it, so the leaf of the
    /// chain is the act that was refused rather than a guess about capacity.
    ReadOnly { region: RegionId },
}

impl DeclineCause {
    /// Whether asking again could change the answer, as opposed to the region needing something else.
    ///
    /// A declined request is retryable in exactly two shapes: another member holds a word that will
    /// come free, or room will exist once somebody releases a block, claims an item, grows the region,
    /// or fills the seat the room is held for. Every other arm is information rather than a transient —
    /// most sharply `ReadOnly`, which nothing any member of this mapping can do will turn into a
    /// `yes`, because the refusal is about the mapping the caller came through.
    pub const fn is_retryable(&self) -> bool {
        matches!(self, Self::Busy { .. } | Self::NoRoom { .. })
    }

    /// Bundles this cause with the value the caller keeps.
    pub const fn decline<T>(self, value: T) -> Decline<T> {
        Decline {
            cause: self,
            value: Some(value),
        }
    }

    /// Bundles this cause with no value: nothing came out of the request, so nothing comes back.
    pub const fn declined<T>(self) -> Decline<T> {
        Decline {
            cause: self,
            value: None,
        }
    }
}

impl core::fmt::Display for DeclineCause {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooWide => f.write_str(
                "the request is wider than any block this authority hands out, so only a smaller one fits",
            ),
            Self::NoRoom { waits } => {
                write!(f, "nothing was free where the request needed room, so {waits}")
            }
            Self::Busy {
                member: Some(member),
            } => write!(
                f,
                "member {member} holds the authority word this request needed, so a retry may take it"
            ),
            Self::Busy { member: None } => f.write_str(
                "another member won the word this request needed, and the race names no holder",
            ),
            Self::Closed => f.write_str("the shared layout is closed to this kind of request"),
            Self::Corrupted => f.write_str(
                "an authority word names a state no participant writes, so the region needs adjudication",
            ),
            Self::Poisoned => f.write_str(
                "a mutation word is poisoned, which recovery adjudicates rather than a retry",
            ),
            Self::Overflow => {
                f.write_str("the request's own layout arithmetic overflows, so nothing fits it")
            }
            Self::ReadOnly { region } => write!(
                f,
                "region {region:?} is mapped read-only here, so the member this write needs cannot be admitted"
            ),
        }
    }
}

impl core::error::Error for DeclineCause {}

/// A declined request, with whatever of the caller's value still exists.
///
/// One shape covers every authority that answers "no" while holding something of the caller's — a
/// block request, a queue reservation, a heap mutation — because the answer always has the same two
/// halves: what the authority knows (`cause`) and what the caller keeps (`value`). `value` is an
/// option because one call can answer both ways: a channel send that finds the queue full hands the
/// transfer back, while one whose block was claimed by another member between reservation and
/// publication has nothing left to hand. That distinction belongs beside the cause, not in a second
/// error type, which is what the shapes this replaced had to do.
///
/// The value is not part of why the answer is what it is, so the reporting layer stays free of it:
/// [`DeclineCause`] prints, boxes and chains whatever `T` is, and a payload that cannot render itself
/// still yields a readable refusal.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Decline<T> {
    pub cause: DeclineCause,
    /// The value the request did not consume, or `None` when the decline says it is gone.
    pub value: Option<T>,
}

// `T` is the caller's own value coming back rather than part of the diagnosis, so `Debug` is written by
// hand: a derived one would tie the refusal's report to whether the payload happens to be printable.
// The payload's name is said instead of its contents — once, for the whole decline family, where the
// types this one replaces each said it separately.
impl<T> core::fmt::Debug for Decline<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let value = match &self.value {
            Some(value) => core::any::type_name_of_val(value),
            None => return write!(f, "Decline {{ cause: {:?}, value gone }}", self.cause),
        };
        write!(f, "Decline {{ cause: {:?}, value: {} }}", self.cause, value)
    }
}

impl<T> core::fmt::Display for Decline<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(&self.cause, f)
    }
}

impl<T> core::error::Error for Decline<T> {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.cause)
    }
}

/// Exposes one live contiguous memory extent.
///
/// # Safety
///
/// `start_ptr` and `size` must remain valid for the implementation's lifetime.
pub unsafe trait MemOps {
    fn start_ptr(&self) -> *const u8;
    fn size(&self) -> usize;

    #[inline]
    unsafe fn start_mut_ptr(&self) -> *mut u8 {
        self.start_ptr().cast_mut()
    }

    #[inline]
    unsafe fn offset<T: ?Sized>(&self, ptr: *const T) -> usize {
        unsafe { ptr.byte_offset_from_unsigned(self.start_ptr()) }
    }
}

unsafe impl<M: MemOps> MemOps for &M {
    fn start_ptr(&self) -> *const u8 {
        (*self).start_ptr()
    }

    fn size(&self) -> usize {
        (*self).size()
    }
}

/// A span inside one mapping: where it starts, then how long it is.
///
/// `talc::Meta` holds one of these and is a [`Repr`](crate::msg::Repr), so both legs reach a peer as
/// bytes; the order between them is a promise, not a layout choice.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddrSpan<T> {
    pub start_offset: T,
    pub size: T,
}

impl<T> AddrSpan<T> {
    pub const fn new(start_offset: T, size: T) -> Self {
        Self { start_offset, size }
    }
}

impl AddrSpan<usize> {
    pub const fn null() -> Self {
        Self::new(0, 0)
    }

    pub const fn is_null(&self) -> bool {
        self.start_offset == 0 || self.size == 0
    }

    pub const unsafe fn as_nonnull(&self, base: *const u8) -> NonNull<u8> {
        unsafe { NonNull::new_unchecked(base.add(self.start_offset).cast_mut()) }
    }
}

/// Alignment every mapping of a region is guaranteed to have in every process that maps it.
///
/// A pool's class blocks are `bytes`-aligned against the absolute address the allocator happened to
/// get, and a peer reading the same record offset only reproduces that alignment if its own base has
/// the same residue — which is what `header::recorded_at` refuses. `os::unix` arranges the residue by
/// placing the file at an address on this bound instead of hoping `mmap` chose one, and
/// `pool::Geometry::build` refuses a wider class than the bound, so the promise is never one a peer
/// cannot keep. Because the bound names what the host's mapping can promise, it is the platform's own
/// property: a caller that wants one geometry everywhere keeps its classes inside the smallest of
/// them. Windows takes the granularity its view placement is documented to round addresses to, which
/// is coarser than any class the Unix-side tests here lay out, and has not been executed. A test
/// backend that hands every session a slice of one buffer cannot vary its residue, so it exercises
/// neither the arrangement nor the refusal.
///
/// This is also the number a `Source` written outside the crate has to meet, which is why it is
/// published rather than only assumed by a safety note: a port arranges its own base, and the multiple
/// of the extent alignment a region recorded is what a joining member is refused for missing.
#[cfg(unix)]
pub const REGION_ALIGN: usize = 1 << 21;
#[cfg(not(unix))]
pub const REGION_ALIGN: usize = 1 << 16;

/// Linear owner of one process-local mapping.
pub struct Map {
    start: NonNull<u8>,
    len: usize,
    access: Access,
    release: Option<unsafe fn(NonNull<u8>, usize) -> bool>,
}

impl core::fmt::Debug for Map {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Map")
            .field("start", &self.start)
            .field("len", &self.len)
            .field("access", &self.access)
            .finish()
    }
}

unsafe impl MemOps for Map {
    #[inline]
    fn start_ptr(&self) -> *const u8 {
        self.start.as_ptr()
    }

    #[inline]
    fn size(&self) -> usize {
        self.len
    }
}

impl Drop for Map {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = unsafe { release(self.start, self.len) };
        }
    }
}

impl Map {
    /// Creates an owner for an externally established mapping.
    ///
    /// # Safety
    ///
    /// `start..start + len` must remain exclusively owned and valid for the
    /// declared access until `release` is invoked. `release` must release that
    /// exact mapping and must be safe to call exactly once from any thread.
    pub const unsafe fn from_raw_parts(
        start: NonNull<u8>,
        len: usize,
        access: Access,
        release: unsafe fn(NonNull<u8>, usize) -> bool,
    ) -> Self {
        Self {
            start,
            len,
            access,
            release: Some(release),
        }
    }

    /// Owns an extent this process was given and can never give back.
    ///
    /// This is the shape of a region that lives in the program's own image: there is no host object to
    /// unmap, so no release function exists to name, and dropping the owner simply lets the bytes go.
    /// A caller that has a real mapping to release uses [`Map::from_raw_parts`] instead — declaring an
    /// extent static when it is not leaves whatever the host allocated behind mapped after this owner
    /// is gone.
    ///
    /// # Safety
    ///
    /// `start..start + len` must stay valid, carry no [`REGION_ALIGN`] residue, and be exclusively
    /// owned for the declared access for as long as this process lives. Nothing here can keep another
    /// owner out, and nothing here will try to release it.
    pub const unsafe fn from_static(start: NonNull<u8>, len: usize, access: Access) -> Self {
        Self {
            start,
            len,
            access,
            release: None,
        }
    }

    #[inline]
    pub fn permits(&self, access: Access) -> Result<(), Error> {
        if !self.access.contains(access) {
            return Err(Error::PermissionDenied { requested: access });
        }
        Ok(())
    }

    fn close(mut self) -> bool {
        // A static extent has nothing to give back, so closing it succeeds by being let go.
        let Some(release) = self.release.take() else {
            return true;
        };
        unsafe { release(self.start, self.len) }
    }

    /// Where a `T` placed at `offset` in fact starts and ends inside this extent.
    ///
    /// Address arithmetic only: it demands no access and hands back a shared pointer, because a
    /// participant that can never claim a slot still has to answer this question, and its answer has
    /// to be the writer's rather than its own.
    #[inline]
    unsafe fn layout_at<T: Layout>(&self, offset: usize) -> Result<(*const T, usize), Error> {
        let align = core::mem::align_of::<T>();
        let start = self.start_ptr().addr();
        let candidate = start.checked_add(offset).ok_or(Error::ArithmeticOverflow)?;
        let aligned_address = candidate
            .checked_add(align - 1)
            .map(|value| value & !(align - 1))
            .ok_or(Error::ArithmeticOverflow)?;
        let end = aligned_address
            .checked_add(core::mem::size_of::<T>())
            .ok_or(Error::ArithmeticOverflow)?;
        let aligned = aligned_address
            .checked_sub(start)
            .ok_or(Error::ArithmeticOverflow)?;
        let next = end.checked_sub(start).ok_or(Error::ArithmeticOverflow)?;
        if next > self.size() {
            return Err(Error::UnenoughSpace {
                requested: next,
                allocated: self.size(),
            });
        }
        Ok((unsafe { self.start_ptr().add(aligned) }.cast::<T>(), next))
    }

    #[inline]
    unsafe fn reserve<T: Layout>(&self, offset: usize) -> Result<(*mut T, usize), Error> {
        self.permits(Access::WRITE)?;
        let (pointer, next) = unsafe { self.layout_at::<T>(offset) }?;
        #[cfg(feature = "tracing")]
        tracing::debug!("[Area]: reserve offset, old {}, new {}", offset, next);
        Ok((pointer.cast_mut(), next))
    }

    #[inline]
    unsafe fn commit<T: AdmitLayout>(
        &self,
        header: *mut T,
        conf: T::Config,
        ctx: LayoutContext,
    ) -> Result<NonNull<T>, Error> {
        self.permits(Access::WRITE)?;
        unsafe { T::admit(header, conf, ctx, None) }
            .map(|()| unsafe { NonNull::new_unchecked(header) })
            .map_err(refused)
    }
}

/// The one place an admission failure becomes a mapping failure.
fn refused(error: RecordAdmitError) -> Error {
    match error {
        RecordAdmitError::Contention => Error::Contention,
        RecordAdmitError::Closed => Error::LayoutClosed,
        RecordAdmitError::DuplicateMember => Error::DuplicateAttachment,
        RecordAdmitError::InvalidHeader => Error::InvalidHeader,
        RecordAdmitError::Mismatch(field) => Error::LayoutMismatch(field),
    }
}

/// Which region a published root says it is, and whether this build may read it.
///
/// These are the two steps of root admission that only read: no reserve, no admission CAS, no member
/// claim, so nothing here demands an `Access::WRITE` mapping. What it shares with the writer is the
/// verifier — `inspect_at` is what the writer's `admit` reaches through `mismatch_at` before it claims
/// anything — so the two cannot disagree about what a root claims, and a reader needs no mechanism of
/// its own to find out.
pub(crate) fn read_root(map: &Map) -> Result<RegionId, Error> {
    let (header, _) = unsafe { map.layout_at::<RootHeader>(0) }?;
    let region = unsafe { RootHeader::published_region_at(header) }.ok_or(Error::InvalidHeader)?;
    let ctx = LayoutContext {
        region,
        offset: 0,
        allow_init: false,
    };
    unsafe { RootHeader::inspect_at(header, ctx) }
        .map(|()| region)
        .map_err(refused)
}

/// A standpoint on a region this process reads and never joins.
///
/// `discover` answers the two questions a member's entrance answers — which region does this root claim,
/// and may this build read it — and stops there. What it does not do is everything a member does after
/// them: no reserve, no admission CAS, no claimed slot. That is also why nothing here needs a rule about
/// not writing. The methods that write live behind a member, and this type holds none, so a caller
/// cannot ask for one; the roster it reads is byte-for-byte the roster it found.
pub struct Observer {
    map: Map,
    region: RegionId,
}

impl core::fmt::Debug for Observer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Observer")
            .field("region", &self.region)
            .field("access", &self.map.access)
            .finish()
    }
}

// Safety: an `Observer` names bytes only through shared loads of types that are `Sync` by the [`Layout`]
// contract, and it never hands out a `*mut`, a `&mut`, or a member slot, so two threads that hold one
// cannot be about to disagree over anything the region stores. Moving it moves the release obligation
// with it, which is what [`Source`]'s safety note requires of every owner of a mapping — the same
// argument `Region` makes for itself, minus the writes `Region` can perform and this type cannot reach.
unsafe impl Send for Observer {}
unsafe impl Sync for Observer {}

impl Observer {
    /// Take a standpoint on the region `source` maps, without taking a part in it.
    ///
    /// Seam A is reused exactly as a member reuses it: `source` supplies the extent and `request` the
    /// access declaration, and no backend type or error survives the call. Nothing past that seam is —
    /// which is the only substantive difference between this entrance and a member's.
    ///
    /// There is deliberately no `RegionAdmission` parameter: one of its arms creates, and creating
    /// asserts a commit. An entrance that promises no commit must not be eligible to be asked for one.
    pub fn discover<S: Source>(
        source: S,
        request: Request,
    ) -> Result<Self, ObserverError<S::Error>> {
        let map = source.map(request).map_err(ObserverError::Source)?;
        // Every step below performs real loads, so the declaration this mapping was made under has to
        // say the bytes may be read. A wider ask is refused by the provider, and a narrower one —
        // `Access::WRITE` alone — would otherwise be read straight through protection the host set.
        map.permits(Access::READ).map_err(ObserverError::Refused)?;
        let region = read_root(&map).map_err(ObserverError::Refused)?;
        Ok(Self { map, region })
    }

    /// Which region this standpoint is on.
    ///
    /// This is the only exit for the id: an observer is not a member, so it has no peer to ask, and the
    /// address details it read its way to are not something it may hand on.
    pub fn region(&self) -> RegionId {
        self.region
    }

    /// The access declaration the mapping underneath this was made under.
    ///
    /// Reported rather than assumed, because the two hosts mean different things by the same bits: this
    /// says what the process was granted, not what the object underneath allows.
    pub fn access(&self) -> Access {
        self.map.access
    }

    /// What the published `L` at `at` says it is, verified against the records this region wrote.
    ///
    /// This is the reader's half of the verification the header tests aim at, and it is compiled only
    /// for them: no entrance a caller can reach asks a standpoint that holds no slot to read a record's
    /// own shape, so publishing it would hand out a name nobody can spell (`header::Layout` is not a
    /// downstream implementation point) and leaving it in a release build would be code without a
    /// producer. Giving an outside monitor that read is roadmap M4's addition, not a re-exposure.
    ///
    /// The answer is the layout's own information value — a copy, not a reference into the mapping. That
    /// is not a convenience: `&L` would put `init`, `attach` and `recover` under a reader's fingers,
    /// which are the three writer methods this type exists to make unaskable, and it would make the rule
    /// depend on which layout was supplied rather than on what a reader may do with any of them.
    ///
    /// The verification is the writer's: the same `inspect_at` that a member's entrance reaches through
    /// `mismatch_at` before it claims anything, so a reader and a member cannot disagree about what a
    /// record claims, and a record that lies about its region, offset or shape is refused here as it is
    /// refused there.
    #[cfg(test)]
    pub(crate) fn read<L: Layout>(&self, at: usize) -> Result<L::Info, Error> {
        let (header, _) = unsafe { self.map.layout_at::<crate::header::RcHeader<L>>(at) }?;
        let ctx = LayoutContext {
            region: self.region,
            offset: unsafe { self.map.offset(header) } as u64,
            allow_init: false,
        };
        unsafe { crate::header::RcHeader::<L>::inspect_at(header, ctx) }.map_err(refused)
    }
}

/// Why a mapping never became a standpoint on a region.
///
/// Two arms because there are two owners of the refusal: the provider of the bytes, and the root the
/// bytes hold. Nothing in here is a member's refusal — a reader is never turned away for want of a slot,
/// which is the whole reason this type exists.
#[derive(Debug)]
pub enum ObserverError<E> {
    /// The provider refused to map the extent, so there were no bytes to read.
    Source(E),
    /// The bytes arrived, and what sits at their front does not answer as a region this build may read.
    Refused(Error),
}

impl<E> core::fmt::Display for ObserverError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Source(_) => f.write_str("the provider refused the mapping"),
            Self::Refused(error) => {
                write!(f, "the mapped bytes are not a readable region: {error}")
            }
        }
    }
}

#[cfg(feature = "std")]
impl<E: std::error::Error + 'static> std::error::Error for ObserverError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Source(error) => Some(error),
            Self::Refused(error) => Some(error),
        }
    }
}

/// Immutable process-local authority for one mapped shared-memory region.
///
/// The mapping backend is erased after successful root admission. The only
/// retained backend operation is final unmap.
pub(crate) struct Region {
    map: Option<Map>,
    header: NonNull<RootHeader>,
    member: Member,
    region: RegionId,
    detached: bool,
}

// Safety: Region exposes only atomic root operations to a shared holder, and its typed projections
// require `T: Sync`. `reserve` is the one method that hands out a mutable raw pointer from `&self`,
// and sharing it is sound because a pointer from it is worthless until `commit` wins the layout's
// admission CAS — the permission bits only say the mapping may be written, and that CAS is what
// says this participant may write it. Drop has unique ownership, and Map admission requires its
// release function to be callable from any owning thread.
unsafe impl Send for Region {}
unsafe impl Sync for Region {}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnmapCloseError {
    InUse,
    UnmapFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProjectionError {
    ReadOnly,
    ArithmeticOverflow,
    OutOfBounds,
    Misaligned,
    Admission(RecordAdmitError),
}

unsafe impl MemOps for Region {
    #[inline]
    fn start_ptr(&self) -> *const u8 {
        self.map().start_ptr()
    }

    #[inline]
    fn size(&self) -> usize {
        self.map().size()
    }
}

impl core::fmt::Debug for Region {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Region")
            .field("map", &self.map)
            .field("region", &self.region)
            .finish()
    }
}

impl Drop for Region {
    fn drop(&mut self) {
        self.detach();
        let _ = self.unmap();
    }
}

impl Region {
    /// Admits this process as a member of the region `map` extends over.
    ///
    /// The four steps are not four reads: reserving, committing and claiming a slot write, while
    /// finding out which region this is and whether its layout is readable is [`read_root`]. Nothing
    /// here reaches that read without also writing, which is why the participant that holds no slot and
    /// wants only what a published root says is admitted by [`Observer::discover`], not by an arm of
    /// `admission`.
    fn new(
        map: Map,
        admission: RegionAdmission,
        promised: Option<Peer>,
    ) -> Result<(Arc<Self>, usize), Error> {
        let (ptr, offset) = unsafe { map.reserve::<RootHeader>(0) }?;
        let (region, allow_init) = match admission {
            RegionAdmission::Create(id) => (id, true),
            RegionAdmission::Expect(id) => (id, false),
        };
        let ctx = LayoutContext {
            region,
            offset: 0,
            allow_init,
        };
        let header = unsafe { map.commit(ptr, (), ctx) }?;
        let root = &unsafe { header.as_ref() }.inner;
        let member = match promised {
            // Arriving means taking up the one slot somebody promised this generation, and nobody
            // else's: the compare-exchange is what says the promise still stands.
            Some(promised) => {
                let member = promised.0;
                if root.materialise(member) {
                    member
                } else {
                    return Err(Error::ArrivalRefused);
                }
            }
            None => match root.join() {
                Claim::Taken(member) => member,
                Claim::Promised => {
                    return Err(Error::NoRoom {
                        waits: RoomWait::Promise,
                    });
                }
                Claim::Full => {
                    return Err(Error::NoRoom {
                        waits: RoomWait::Grow,
                    });
                }
            },
        };
        Ok((
            Arc::new(Self {
                map: Some(map),
                header,
                member,
                region,
                detached: false,
            }),
            offset,
        ))
    }

    fn header(&self) -> &RootHeader {
        unsafe { self.header.as_ref() }
    }

    fn map(&self) -> &Map {
        self.map.as_ref().expect("live region owns its map")
    }

    fn permits(&self, access: Access) -> Result<(), Error> {
        self.map().permits(access)
    }

    unsafe fn reserve<T: Layout>(&self, offset: usize) -> Result<(*mut T, usize), Error> {
        unsafe { self.map().reserve::<T>(offset) }
    }

    unsafe fn commit<T: AdmitLayout>(
        &self,
        pointer: *mut T,
        conf: T::Config,
        ctx: LayoutContext,
    ) -> Result<NonNull<T>, Error> {
        self.permits(Access::WRITE)?;
        unsafe { T::admit(pointer, conf, ctx, Some(self.member.slot)) }
            .map(|()| unsafe { NonNull::new_unchecked(pointer) })
            .map_err(refused)
    }

    fn unmap(&mut self) -> bool {
        let Some(map) = self.map.take() else {
            return true;
        };
        map.close()
    }

    fn detach(&mut self) {
        if self.detached {
            return;
        }
        let _ = self.header().inner.leave(self.member);
        self.detached = true;
    }

    #[cfg(test)]
    pub(crate) fn close(region: Arc<Self>) -> Result<(), UnmapCloseError> {
        let mut region = Arc::try_unwrap(region).map_err(|_| UnmapCloseError::InUse)?;
        region.detach();
        if region.unmap() {
            Ok(())
        } else {
            Err(UnmapCloseError::UnmapFailed)
        }
    }

    pub fn peer(&self) -> Peer {
        Peer(self.member)
    }

    /// Marks an exact participant generation as permanently unable to access
    /// this region.
    ///
    /// # Safety
    ///
    /// The caller must know that the process represented by `peer` can never
    /// again access this mapping. A timeout, task cancellation, or thread exit
    /// is not sufficient evidence.
    pub(crate) unsafe fn mark_dead(&self, peer: Peer) -> Option<Member> {
        let root = &self.header().inner;
        (root.mark_dead(peer.0) || root.is_dead(peer.0)).then_some(peer.0)
    }

    pub(crate) fn release_dead(&self, member: Member) -> bool {
        self.header().inner.release_dead(member)
    }

    /// Hold a member slot for a process this one has not started yet.
    pub(crate) fn reserve_member(&self) -> Claim {
        self.header().inner.reserve()
    }

    /// Give back a slot the promised process never arrived in.
    pub(crate) fn cancel_member(&self, member: Member) -> bool {
        self.header().inner.cancel(member)
    }
}

/// Which member of a shared region a sentence is about: its directory slot, under the generation that
/// currently fills it. Which *process* a handover came from is the other question, and a host answers it
/// through `os::Provenance` rather than here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Peer(Member);

impl Peer {
    /// Reconstructs a peer from its wire parts without validation.
    ///
    /// # Safety
    ///
    /// This is a raw naming capability: a bare port that holds a static roster must be able to name
    /// a peer by slot and generation. Validation of those parts (bounds, liveness) is the caller's
    /// obligation, not this constructor's — see `Port::from_parts` for the validated counterpart.
    pub const fn from_parts(slot: u8, generation: usize) -> Self {
        Self(Member { slot, generation })
    }

    pub const fn slot(self) -> u8 {
        self.0.slot
    }

    pub const fn generation(self) -> usize {
        self.0.generation
    }
}

pub(crate) struct Reservation<'a, T: AdmitLayout> {
    layout: &'a mut Build,
    offset: usize,
    next: usize,
    _marker: PhantomData<fn() -> T>,
}

impl<T: AdmitLayout> Reservation<'_, T> {
    #[inline]
    pub fn remaining_after(&self) -> usize {
        self.layout.size().saturating_sub(self.next)
    }

    pub fn commit(self, conf: T::Config) -> Result<Mapped<T>, Error> {
        self.layout.commit_reserved(self.offset, self.next, conf)
    }
}

pub(crate) struct Build {
    area: Arc<Region>,
    offset: usize,
    poisoned: bool,
}

unsafe impl MemOps for Build {
    #[inline]
    fn start_ptr(&self) -> *const u8 {
        self.area.start_ptr()
    }

    #[inline]
    fn size(&self) -> usize {
        self.area.size()
    }
}

impl Build {
    #[inline]
    pub(crate) fn new(map: Map, admission: RegionAdmission) -> Result<Self, Error> {
        Self::establish(map, admission, None)
    }

    /// Build as the member a live participant promised, rather than as whoever claims a slot next.
    pub(crate) fn arriving_as(
        map: Map,
        admission: RegionAdmission,
        promised: Peer,
    ) -> Result<Self, Error> {
        Self::establish(map, admission, Some(promised))
    }

    fn establish(
        map: Map,
        admission: RegionAdmission,
        promised: Option<Peer>,
    ) -> Result<Self, Error> {
        let (area, offset) = Region::new(map, admission, promised)?;
        Ok(Self {
            area,
            offset,
            poisoned: false,
        })
    }

    pub fn region(&self) -> RegionId {
        self.area.region
    }

    #[inline]
    pub(crate) fn reserve<T: AdmitLayout>(&mut self) -> Result<Reservation<'_, T>, Error> {
        if self.poisoned {
            return Err(Error::PoisonedComposition);
        }
        let (ptr, next) = unsafe { self.area.reserve::<T>(self.offset) }?;
        let offset = unsafe { self.area.offset(ptr) };
        Ok(Reservation {
            layout: self,
            offset,
            next,
            _marker: PhantomData,
        })
    }

    fn commit_reserved<T: AdmitLayout>(
        &mut self,
        offset: usize,
        expected_next: usize,
        conf: T::Config,
    ) -> Result<Mapped<T>, Error> {
        if self.poisoned {
            return Err(Error::PoisonedComposition);
        }
        let (ptr, next) = match unsafe { self.area.reserve::<T>(self.offset) } {
            Ok(value) => value,
            Err(error) => {
                self.poisoned = true;
                return Err(error);
            }
        };
        if unsafe { self.area.offset(ptr) } != offset || next != expected_next {
            self.poisoned = true;
            return Err(Error::InvalidHeader);
        }
        let ctx = LayoutContext {
            region: self.region(),
            offset: offset as u64,
            allow_init: true,
        };
        let ptr = match unsafe { self.area.commit::<T>(ptr, conf, ctx) } {
            Ok(ptr) => ptr,
            Err(error) => {
                self.poisoned = true;
                return Err(error);
            }
        };
        self.offset = next;
        let handle = unsafe { Mapped::from_raw(Arc::clone(&self.area), ptr) };
        Ok(handle)
    }

    #[inline]
    pub(crate) fn push<T: AdmitLayout>(&mut self, conf: T::Config) -> Result<Mapped<T>, Error> {
        self.reserve::<T>()?.commit(conf)
    }

    #[cfg(test)]
    pub(crate) fn header(&self) -> &RootHeader {
        self.area.header()
    }

    #[cfg(test)]
    pub(crate) fn close(self) -> Result<(), UnmapCloseError> {
        Region::close(self.area)
    }
}

pub(crate) struct Mapped<T: AdmitLayout> {
    handle: Arc<Region>,
    ptr: NonNull<T>,
    attached: bool,
}
// Safety: Region erasure admits only cache-coherent process-shared mappings, and the projected
// layout is what makes cross-thread access sound: `Mapped<T>` itself is Sync only for `T: Sync`,
// and the only other way this type reaches memory — `ref_at` — is bounded the same way, so a
// projection cannot hand out a shared reference to a type the promise was never made about.
// `Sync` is not restated as a bound because `AdmitLayout: Layout: Sync` already carries it.
unsafe impl<T: AdmitLayout> Send for Mapped<T> {}
unsafe impl<T: AdmitLayout> Sync for Mapped<T> {}

impl<T: AdmitLayout + core::fmt::Debug> core::fmt::Debug for Mapped<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Mapped")
            .field("handle", &self.handle)
            .field("ptr", &self.ptr)
            .finish()
    }
}

impl<T: AdmitLayout> Drop for Mapped<T> {
    fn drop(&mut self) {
        if self.attached {
            unsafe { T::leave(self.ptr.as_ptr(), self.handle.member.slot) };
        }
    }
}

impl<T: AdmitLayout> Deref for Mapped<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref() }
    }
}

impl<T: AdmitLayout> Mapped<T> {
    pub(crate) fn pointer(&self) -> NonNull<T> {
        self.ptr
    }

    pub(crate) fn disarm_detach(&mut self) {
        self.attached = false;
    }

    pub fn region(&self) -> RegionId {
        self.handle.region
    }

    pub fn peer(&self) -> Peer {
        self.handle.peer()
    }

    pub(crate) unsafe fn mark_dead(&self, peer: Peer) -> Option<Member> {
        unsafe { self.handle.mark_dead(peer) }
    }

    pub(crate) fn release_dead(&self, member: Member) -> bool {
        self.handle.release_dead(member)
    }

    pub(crate) fn reserve_member(&self) -> Claim {
        self.handle.reserve_member()
    }

    pub(crate) fn cancel_member(&self, member: Member) -> bool {
        self.handle.cancel_member(member)
    }

    pub(crate) fn offset_of(
        &self,
        pointer: NonNull<u8>,
        size: usize,
    ) -> Result<usize, ProjectionError> {
        let offset = pointer
            .as_ptr()
            .addr()
            .checked_sub(self.handle.start_ptr().addr())
            .ok_or(ProjectionError::OutOfBounds)?;
        let end = offset
            .checked_add(size)
            .ok_or(ProjectionError::ArithmeticOverflow)?;
        (end <= self.handle.size())
            .then_some(offset)
            .ok_or(ProjectionError::OutOfBounds)
    }

    /// Projects `U` at `offset` inside this mapping.
    ///
    /// `U: Sync` is part of the contract, not a convenience: it is `Mapped<T>: Sync` that lets two
    /// threads hold this value at once, and a projection hands out `&U` for a type the compiler never
    /// saw, so a `U` with interior mutability would be shared through a promise made about `T`.
    pub(crate) unsafe fn ref_at<U: Sync>(&self, offset: usize) -> Result<&U, ProjectionError> {
        let end = offset
            .checked_add(core::mem::size_of::<U>())
            .ok_or(ProjectionError::ArithmeticOverflow)?;
        if end > self.handle.size() {
            return Err(ProjectionError::OutOfBounds);
        }
        let pointer = unsafe { self.handle.start_ptr().add(offset) }.cast::<U>();
        if !pointer.is_aligned() {
            return Err(ProjectionError::Misaligned);
        }
        Ok(unsafe { &*pointer })
    }
}

impl<T: crate::header::Layout> Mapped<crate::header::RcHeader<T>> {
    pub(crate) fn recover_member(&self, slot: u8) -> bool {
        unsafe { self.ptr.as_ref() }.recover_member(slot)
    }

    pub(crate) fn has_member(&self, slot: u8) -> bool {
        unsafe { self.ptr.as_ref() }.has_member(slot)
    }
}

impl<T: AdmitLayout> Mapped<T> {
    unsafe fn from_raw(handle: Arc<Region>, ptr: NonNull<T>) -> Self {
        Self {
            handle,
            ptr,
            attached: true,
        }
    }

    pub(crate) fn admit_at<U: AdmitLayout>(
        &self,
        offset: usize,
        conf: U::Config,
        allow_init: bool,
    ) -> Result<Mapped<U>, ProjectionError> {
        if self.handle.permits(Access::WRITE).is_err() {
            return Err(ProjectionError::ReadOnly);
        }
        let layout = <U as Layout>::storage(&conf).ok_or(ProjectionError::Admission(
            crate::header::RecordAdmitError::InvalidHeader,
        ))?;
        let end = offset
            .checked_add(layout.size())
            .ok_or(ProjectionError::ArithmeticOverflow)?;
        if end > self.handle.size() {
            return Err(ProjectionError::OutOfBounds);
        }
        let pointer = unsafe { self.handle.map().start_mut_ptr().add(offset) }.cast::<U>();
        if pointer.addr() % layout.align() != 0 {
            return Err(ProjectionError::Misaligned);
        }
        let offset = u64::try_from(offset).map_err(|_| ProjectionError::ArithmeticOverflow)?;
        let ctx = LayoutContext {
            region: self.handle.region,
            offset,
            allow_init,
        };
        unsafe { U::admit(pointer, conf, ctx, Some(self.handle.member.slot)) }
            .map_err(ProjectionError::Admission)?;
        Ok(unsafe { Mapped::from_raw(self.handle.clone(), NonNull::new_unchecked(pointer)) })
    }
}
