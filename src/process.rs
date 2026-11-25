//! Owns the frozen ceilings a handoff is measured against — `MAX_BOOTSTRAP`, `MAX_RESOURCES`, the
//! magic a rendezvous carries and the keys a child is told things through — and the linear ownership of
//! exactly one child. Bytes enter only through `Bootstrap::new`, and a promised member slot has exactly
//! two entrances: the `Seat` of the process holding the promise, and the environment text of the
//! process it was promised to, which is what keeps an `Introduction` from being a number a caller
//! typed. A child's terminal state is reached only through `try_wait`, `wait` or `kill_wait`, and the
//! status is then memoized, which is what makes an `Exit` a proof rather than another poll.
//!
//! Beside the proof sits a witness: `exited` asks the host to keep an eye on the child without taking it
//! off the owner's hands, because the death is the one thing a caller has to be able to wait for while
//! the status is still unreaped. Linux answers with a `pidfd` and the other arms refuse; waiting on the
//! answer lives in `crate::runtime`, which is the lane that parks a task rather than a thread.
//!
//! `Arrival` and `Death` are the two compositions that turn a witness into authority over a member, and
//! this file holds only their shapes: the frame a receipt travels in is written by an `os` leg, and so is
//! the credit the host puts beside it. `Death::witnessed` is where a receipt read from one process is
//! compared with the exit of another, which is what makes the pid in a receipt an obligation no test here
//! can catch — a `Death` assembled from a report the kernel did not credit to the process it reaped would
//! mark a live member dead, and `Session::recover` would call that recovery. Its sibling
//! `Death::credited` skips that comparison for a reason worth stating: its two halves leave one `recvmsg`
//! — the report and the `pidfd` the kernel wrote beside it for the writer — so there is no second channel
//! to reconcile, and what binds them is not a number a caller typed but the fact that neither half has a
//! constructor a caller can reach.
//!
//! `Drop` has to reap a child nobody waited on. The suite covers waiting, killing and a spawn that
//! failed, and it reaches the drop arm only sideways — a witness that outlives the supervisor whose
//! `Drop` killed the child is the one thing that fails if that arm stops reaping — so what that arm is
//! checked against remains a single narrow case. `MAX_RESOURCES` is enforced on the receiving side, not
//! here.
use std::{
    io,
    process::{Child, Command, ExitStatus},
};

#[cfg(all(feature = "process", any(unix, windows)))]
use std::env;

#[cfg(target_os = "linux")]
use std::{
    os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd},
    sync::Arc,
};

use crate::{mem::Peer, place::RegionId};

pub const MAX_BOOTSTRAP: usize = 4096;
pub const MAX_RESOURCES: usize = 8;
// The front is assembled from these two widths rather than hand-counted: the tag's own width, which
// both tags below are declared at, and the wire width of a count. The total a peer copies stays the
// literal `FRAME_HEADER`, and the assertion under the three ranges is what ties the two together.
const TAG_BYTES: usize = 4;
const COUNT_BYTES: usize = core::mem::size_of::<u16>();
/// The tag a handoff offer starts with.
///
/// Published as data rather than kept as a writer's private constant, because a process written
/// without this crate answers an offer in the same bytes, and until now the only place to read the
/// four of them was `src`. What it sits in is published beside it — [`FRAME_HEADER`] and the three
/// offsets — so the tag is no longer the only readable half of a frame. One thing this is not: the tag
/// is a collision test between two frame kinds, not a version a receiver negotiates.
/// `tests/handover_attestation/labels.rs` pins both labels apart and pins the front's width, so
/// changing one fails a leg before it changes the wire.
pub const HANDOFF_MAGIC: [u8; TAG_BYTES] = *b"EVR1";

/// The tag a child's arrival receipt starts with, on the rendezvous it was granted.
///
/// A second tag rather than `EVR1` with a new field or a zero resource count, because the two frames
/// answer different questions: the offer says what the parent is giving, and the receipt says what the
/// child became. A reader that could accept either would take a receipt as an offer with no resources,
/// and the difference is the one thing the seam has to keep apart. The same [`FRAME_HEADER`] front with
/// the same two count fields, so the grammar a receiver refuses with stays the one
/// `Introduction::announced` already defines — which is what lets a scribbled receipt be refused as
/// the text it is.
///
/// Published beside [`HANDOFF_MAGIC`], and no longer gated on the host: what a receipt begins with is
/// the same on every platform that runs this protocol, and the platform's own choice is how the frame
/// is delivered.
pub const ARRIVAL_MAGIC: [u8; TAG_BYTES] = *b"EVA1";

/// The width of the fixed front both frame kinds carry, published beside the two tags.
///
/// A tag alone does not let a process written without this crate read a frame: knowing a receipt begins
/// with `EVA1` says nothing about where its text starts. The front is this many bytes, and
/// [`FRAME_TAG`], [`FRAME_LEN`] and [`FRAME_COUNT`] say what occupies them, so the payload begins at
/// `FRAME_HEADER` and the two counts a receiver guards it with are readable at a stated offset rather
/// than inferred from a reader's habit. The counts are little-endian `u16`s, which is why
/// [`MAX_BOOTSTRAP`] and [`MAX_RESOURCES`] are the bounds they are.
pub const FRAME_HEADER: usize = 8;
/// Where the tag bytes sit inside the front: `TAG_BYTES` of them, no more and no less.
pub const FRAME_TAG: core::ops::Range<usize> = 0..TAG_BYTES;
/// Where the bootstrap length sits, as `COUNT_BYTES` little-endian bytes.
pub const FRAME_LEN: core::ops::Range<usize> = TAG_BYTES..TAG_BYTES + COUNT_BYTES;
/// Where the resource count sits, as `COUNT_BYTES` little-endian bytes. Zero says a receipt travels
/// alone.
pub const FRAME_COUNT: core::ops::Range<usize> =
    TAG_BYTES + COUNT_BYTES..TAG_BYTES + 2 * COUNT_BYTES;

const _: () = assert!(
    FRAME_TAG.end == FRAME_LEN.start
        && FRAME_LEN.end == FRAME_COUNT.start
        && FRAME_COUNT.end == FRAME_HEADER,
    "the three published offsets have to tile the published width, or a porter reading them lands \
     outside the front"
);

/// Assemble a frame's front: the tag the receiver compares, then the two counts it guards.
///
/// Both host writers go through this, because the two of them hand-rolling the same eight bytes is how
/// a wire agreement stops being one agreement: a frame never crosses from one host's writer to the
/// other's reader, so nothing but a shared owner can tell the two apart.
pub(crate) fn stamp_front(
    tag: [u8; 4],
    bootstrap_len: usize,
    resources: usize,
) -> [u8; FRAME_HEADER] {
    debug_assert!(
        bootstrap_len <= MAX_BOOTSTRAP && resources <= MAX_RESOURCES,
        "the front carries both counts as `u16`, and the published bounds sit far inside that"
    );
    let mut front = [0; FRAME_HEADER];
    front[FRAME_TAG].copy_from_slice(&tag);
    front[FRAME_LEN].copy_from_slice(&(bootstrap_len as u16).to_le_bytes());
    front[FRAME_COUNT].copy_from_slice(&(resources as u16).to_le_bytes());
    front
}

/// Read a frame's front, refusing bytes too short to have one or begun with the other kind's tag.
///
/// One answer for both entrances, and the caller turns `None` into the sentence its own seam reports.
pub(crate) fn read_front(tag: [u8; 4], bytes: &[u8]) -> Option<(usize, usize)> {
    if bytes.len() < FRAME_HEADER || bytes[FRAME_TAG] != tag {
        return None;
    }
    let bootstrap = u16::from_le_bytes(bytes[FRAME_LEN].try_into().ok()?);
    let resources = u16::from_le_bytes(bytes[FRAME_COUNT].try_into().ok()?);
    Some((bootstrap as usize, resources as usize))
}

/// The one key by which a granted rendezvous is announced to the process it was granted to.
///
/// Its value names the rendezvous rather than locating it: on Linux the descriptor number the
/// granting process installed before `exec`, on Windows the pipe instance the listener created.
/// Knowledge of the value is not what authorises a handover — possession of the rendezvous is —
/// so this key carries no secret and the value is safe to place in the child's environment.
#[cfg(all(feature = "process", any(unix, windows)))]
pub(crate) const RENDEZVOUS: &str = "EVERING_RENDEZVOUS";

/// Windows only: the process id of the party that admitted the grant, which the receiver matches
/// against the kernel's own answer for the pipe server. Linux needs no second key because the
/// descriptor it was granted already reports its creator, and `getppid` is the check there.
#[cfg(all(feature = "process", windows))]
pub(crate) const INTRODUCER: &str = "EVERING_INTRODUCER";

/// The key a promised member slot is announced through, absent when nothing was promised.
///
/// A third key rather than a fourth resource on the rendezvous, because the promise is not one of the
/// objects that travel positionally: the count and order `receive` removes them in is a contract with
/// every sender, and a handover that got one more descriptor in front of them would deliver each
/// peer's mapping as somebody else's event. The magic a rendezvous frame starts with stays `EVR1` for
/// the same reason — this adds a claim beside the frame, not a version of it.
#[cfg(all(feature = "process", any(unix, windows)))]
pub(crate) const SEAT: &str = "EVERING_SEAT";

/// The key the introducer's own policy about an unattestable peer is announced through, absent when the
/// introducer said nothing — which is the default of no.
///
/// A fourth key rather than a fifth field of `EVERING_SEAT`, because the seat text is frozen at the
/// four numbers a receiver already refuses at, and this carries no identity to widen that grammar over.
/// It is a policy statement and not a claim about the peer: whoever admits a process says with this key
/// whether a rendezvous that answers nothing about its other end may still complete. The receiver reads
/// the word and never writes one, which is the only reason a handover can complete on a target that has
/// no way to attest anything.
#[cfg(all(feature = "process", any(unix, windows)))]
pub(crate) const TRUST: &str = "EVERING_TRUST";

/// The member slot a parent promised before the process holding it existed, announced to that process.
///
/// A region and one of its member words: [`crate::Session::open_as`] arrives at the slot and
/// generation this names. Only the roster decides. An announcement that no live member is holding
/// stands promised to nobody, so arrival is refused without joining anywhere, and a process that read
/// the number off somebody else's environment gets the same answer as one that made the text up —
/// which is why receiving one proves nothing beyond the text it came in.
///
/// On the sending side there is no way to make one of these from nothing: it comes from
/// [`crate::Seat::introduction`], a conversion a process can only reach for a promise it holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Introduction {
    region: RegionId,
    member: Peer,
}

impl Introduction {
    /// Bind a region to the slot promised in it, which is what the parent's half of an announcement
    /// is made of. `Seat::introduction` is the only caller: the pair comes from a promise one holds,
    /// never from numbers a caller typed.
    pub(crate) const fn new(region: RegionId, member: Peer) -> Self {
        Self { region, member }
    }

    /// The region whose roster the promised slot belongs to.
    pub fn region(&self) -> RegionId {
        self.region
    }

    /// The slot and generation the holding process promised, which is what an arrival names.
    pub fn peer(&self) -> Peer {
        self.member
    }

    /// The environment text this announcement travels as: `region.high,region.low,slot,generation`,
    /// four decimal numbers and three commas, which is everything either side needs and nothing it can
    /// disagree about. A further field would widen what a receiver refuses, so the shape is frozen here
    /// rather than at the seam that reads it.
    pub(crate) fn announcement(&self) -> String {
        format!(
            "{},{},{},{}",
            self.region.high,
            self.region.low,
            self.member.slot(),
            self.member.generation()
        )
    }

    /// Adopt an announcement read out of the environment, or refuse the text as not one.
    ///
    /// Refused rather than guessed: an empty or repeated separator, a field that is not a number, a
    /// fifth field, and a slot beyond the width the roster addresses. A caller then treats a scribbled
    /// claim as no claim at all instead of arriving at a slot nobody named. The generation is not
    /// checked against the width the member word fits it into, because that is the roster's own answer
    /// to give, and a number no word can hold is refused there as surely as a malformed one.
    pub(crate) fn announced(text: &str) -> Option<Self> {
        let mut fields = text.split(',');
        let high = fields.next()?.parse::<u64>().ok()?;
        let low = fields.next()?.parse::<u64>().ok()?;
        let slot = fields.next()?.parse::<u8>().ok()?;
        let generation = fields.next()?.parse::<usize>().ok()?;
        if fields.next().is_some() || slot as usize >= crate::header::PARTICIPANT_CAPACITY {
            return None;
        }
        Some(Self {
            region: RegionId::new(high, low),
            member: Peer::from_parts(slot, generation),
        })
    }
}

/// Whether the grant that admitted this process said a peer it cannot attest may complete the handover.
///
/// The answer comes from [`TRUST`], which only [`Grant::admit`](crate::os::Grant::admit) writes: a
/// receiver that decided this question for itself would be granting itself the authority the policy
/// exists to withhold, and the environment is the receiver's own. Absent is the default of no.
#[cfg(all(feature = "process", any(unix, windows)))]
pub(crate) fn announced_trust() -> io::Result<bool> {
    let announced = match env::var(TRUST) {
        Ok(announced) => Some(announced),
        Err(env::VarError::NotPresent) => None,
        Err(_) => return Err(refused("the announced introduction policy is not text")),
    };
    policy_of(announced.as_deref())
}

/// The two words a grant announces, and the refusal of anything else.
///
/// `0` is the negative written out rather than left out, so a host that exports the key for every
/// process still admits the children it means to. Any other text is refused instead of read as the
/// default: a reader that guessed at a policy word could guess it open, and the whole point of the key
/// is that guessing is not how this answer gets decided.
#[cfg(all(feature = "process", any(unix, windows)))]
fn policy_of(announced: Option<&str>) -> io::Result<bool> {
    match announced {
        None | Some("0") => Ok(false),
        Some("1") => Ok(true),
        _ => Err(refused(
            "the announced introduction policy names no policy this grant writes",
        )),
    }
}

#[cfg(all(feature = "process", any(unix, windows)))]
fn refused(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, reason)
}

/// One process's own report of the member it became, credited by the host to the process that reported.
///
/// A [`Seat`](crate::Seat) announcement is the parent's word about a slot; this is the child's word about
/// what it arrived as, and the two are allowed to disagree — a promised process can join somewhere else,
/// and a receipt naming an identity nobody promised it is refused at the read rather than kept. What the
/// child reports comes out of its own session (`Session::region()` beside `Session::peer()`), not out of
/// the environment it was handed, which is the only reason the text says anything beyond what was asked.
///
/// The pid beside it is not part of the frame: the host names the writer, and a writer cannot choose what
/// it is named by. That is what makes [`Death`] reachable at all, and it is still not a death — this
/// reports who arrived, not who is gone.
///
/// [`crate::os::Attendance::arrival`](crate::os::Attendance::arrival) is the only entrance, so there is no
/// value of this type that was not read off a granted rendezvous.
#[cfg(all(feature = "process", any(unix, windows)))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arrival {
    region: RegionId,
    member: Peer,
    pid: u32,
}

#[cfg(all(feature = "process", any(unix, windows)))]
impl Arrival {
    /// Record what a credited writer reported. The `os` leg that read the frame and asked the host who
    /// wrote it is the only caller, and the pid it passes is the host's answer rather than a field.
    pub(crate) const fn reported(region: RegionId, member: Peer, pid: u32) -> Self {
        Self {
            region,
            member,
            pid,
        }
    }

    /// The region the reported member stands in.
    pub fn region(&self) -> RegionId {
        self.region
    }

    /// The slot and generation the reporting process says it arrived as.
    pub fn peer(&self) -> Peer {
        self.member
    }

    /// The process the host credited with the report.
    pub fn pid(&self) -> u32 {
        self.pid
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bootstrap(Box<[u8]>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TooLarge {
    pub len: usize,
    pub max: usize,
}

impl Bootstrap {
    pub fn new(bytes: impl AsRef<[u8]>) -> Result<Self, TooLarge> {
        let bytes = bytes.as_ref();
        if bytes.len() > MAX_BOOTSTRAP {
            return Err(TooLarge {
                len: bytes.len(),
                max: MAX_BOOTSTRAP,
            });
        }
        Ok(Self(bytes.into()))
    }

    pub fn into_boxed(self) -> Box<[u8]> {
        self.0
    }
}

impl AsRef<[u8]> for Bootstrap {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(all(test, feature = "process", any(unix, windows)))]
#[test]
fn a_granted_rendezvous_is_announced_by_one_frozen_key() {
    assert_eq!(RENDEZVOUS, "EVERING_RENDEZVOUS");
}

#[cfg(all(test, feature = "process", any(unix, windows)))]
#[test]
fn a_promised_seat_is_announced_by_a_key_nobody_else_writes() {
    assert_eq!(SEAT, "EVERING_SEAT");
}

#[cfg(all(test, feature = "process", any(unix, windows)))]
#[test]
fn an_introducers_policy_is_announced_by_a_key_of_its_own() {
    assert_eq!(TRUST, "EVERING_TRUST");
    assert_ne!(
        TRUST, SEAT,
        "the seat text names a member slot, and a key that overwrote it would trade a policy for an identity"
    );
}

#[cfg(all(test, feature = "process", any(unix, windows)))]
#[test]
fn only_the_two_words_a_grant_announces_are_read_as_its_policy() {
    assert!(!policy_of(None).unwrap(), "said nothing is the default no");
    assert!(!policy_of(Some("0")).unwrap());
    assert!(policy_of(Some("1")).unwrap());
    for scribbled in ["", "yes", "1 ", "true", "01"] {
        let error = policy_of(Some(scribbled))
            .err()
            .unwrap_or_else(|| panic!("read {scribbled:?} as a policy nobody wrote"));
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }
}

#[cfg(test)]
#[test]
fn a_promised_seat_survives_the_environment_it_is_announced_in() {
    let widest = crate::header::PARTICIPANT_CAPACITY - 1;
    let cases = [
        (RegionId::new(7, 3), Peer::from_parts(1, 2), "7,3,1,2"),
        (
            RegionId::new(0, u64::MAX),
            Peer::from_parts(widest as u8, usize::MAX),
            "0,18446744073709551615,62,18446744073709551615",
        ),
    ];
    for (region, member, text) in cases {
        let promised = Introduction { region, member };
        assert_eq!(promised.announcement(), text);
        assert_eq!(
            Introduction::announced(text).as_ref(),
            Some(&promised),
            "a slot announced is a slot the receiver can name back"
        );
    }
}

#[cfg(test)]
#[test]
fn an_announcement_that_is_not_four_numbers_arrives_at_nothing() {
    for text in [
        "",
        "1,2,3",
        "1,2,3,4,5",
        "1,2,3,",
        ",2,3,4",
        "1,2,3,4,",
        "1, 2, 3, 4",
        "1,2,3,x",
        "-1,2,3,4",
        "1,2,3,4 ",
        "1,2,63,4",
        "1,2,255,4",
        "1,2,3,4\n5",
        "0x1,2,3,4",
    ] {
        assert_eq!(
            Introduction::announced(text),
            None,
            "{text:?} is not an announcement, and guessing at it would arrive at a slot nobody named"
        );
    }
}

/// Linear ownership of one exact child instance.
pub struct Supervisor {
    child: Child,
    status: Option<ExitStatus>,
}

/// Terminal evidence for the exact child retained by its supervisor.
///
/// The supervisor is held rather than phantom-held because an exit is the half of a death proof that
/// names a *process*: [`Death::witnessed`] compares the pid this reference answers for against the pid
/// the host credited with an arrival, and a value that carried the status alone could not be compared
/// with anything.
pub struct Exit<'a> {
    status: &'a ExitStatus,
    supervisor: &'a Supervisor,
}

/// A witness that the exact child a supervisor owns is gone, held apart from the owner that reaps it.
///
/// [`Supervisor::exited`] is the only way to obtain one, which is what ties it to a process this
/// process admitted rather than to a number that may already name somebody else. It reports the death
/// and nothing about it: the status belongs to the owner, because a witness has to be obtainable while
/// the child is still unreaped, and a value that also claimed to know the exit code would be a second
/// proof of one fact. Waiting on it lives in [`crate::runtime`], which is the lane that can park a task
/// without spending a thread on it.
///
/// Being held apart is also what makes it safe to hold: once the descriptor is open it answers for the
/// child it names, so a witness outliving its supervisor — whose `Drop` kills and reaps — still reports
/// the death rather than parking on a process nobody will reap.
#[derive(Debug)]
pub struct Exited {
    // Non-Linux arms never read this: the only witness they can build is `Recorded`.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    witness: Witness,
}

/// What the host was asked to watch, or the reason nobody needs watching.
#[derive(Debug)]
enum Witness {
    /// The kernel's own reference to one process: it reports readiness when that process is gone, stays
    /// valid while the child waits to be reaped, and cannot be retargeted at a later holder of the same
    /// number. Linux answers with a `pidfd`; no other arm writes a witness at all, so no other arm has
    /// this variant.
    #[cfg(target_os = "linux")]
    Process(Arc<OwnedFd>),
    /// This supervisor had already recorded the status, so the death is this process's own record and
    /// there is nothing left to ask the host. A witness in this state is ready the moment it is waited
    /// on, and never names the host again.
    Recorded,
}

impl Supervisor {
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        command.spawn().map(|child| Self {
            child,
            status: None,
        })
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    #[cfg(windows)]
    pub(crate) fn handle(&self) -> windows_sys::Win32::Foundation::HANDLE {
        use std::os::windows::io::AsRawHandle;
        self.child.as_raw_handle().cast()
    }

    /// A witness that this exact child is gone, taken while this supervisor still owns it.
    ///
    /// Asking after the child was waited on is not an error: such a witness starts out ready, because
    /// the death it would report is already this process's own record, and it never asks the host
    /// anything. Asking while the child is live is where the host's help is needed, and only Linux
    /// answers here — the other arms refuse out loud rather than hand back a value a caller can park
    /// on and never wake. What a witness reports is the death alone: the status stays with `try_wait`,
    /// `wait` and `kill_wait`, which are the operations that reap and therefore the only ones that can
    /// say what the child returned.
    pub fn exited(&self) -> io::Result<Exited> {
        if self.status.is_some() {
            return Ok(Exited {
                witness: Witness::Recorded,
            });
        }
        watch(self.child.id())
    }

    pub fn try_wait(&mut self) -> io::Result<Option<Exit<'_>>> {
        if self.status.is_none() {
            self.status = self.child.try_wait()?;
        }
        Ok(self.status.as_ref().map(|status| Exit {
            status,
            supervisor: &*self,
        }))
    }

    pub fn wait(&mut self) -> io::Result<Exit<'_>> {
        if self.status.is_none() {
            self.status = Some(self.child.wait()?);
        }
        Ok(Exit {
            status: self.status.as_ref().unwrap(),
            supervisor: &*self,
        })
    }

    pub fn kill_wait(&mut self) -> io::Result<Exit<'_>> {
        if self.status.is_none() {
            match self.child.try_wait()? {
                Some(status) => self.status = Some(status),
                None => {
                    if let Err(kill) = self.child.kill() {
                        match self.child.try_wait()? {
                            Some(status) => {
                                self.status = Some(status);
                                return Ok(Exit {
                                    status: self.status.as_ref().unwrap(),
                                    supervisor: &*self,
                                });
                            }
                            None => return Err(kill),
                        }
                    }
                    self.status = Some(self.child.wait()?);
                }
            }
        }
        Ok(Exit {
            status: self.status.as_ref().unwrap(),
            supervisor: &*self,
        })
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        if self.status.is_some() {
            return;
        }
        match self.child.try_wait() {
            Ok(Some(status)) => self.status = Some(status),
            Ok(None) => {
                let _ = self.child.kill();
                self.status = self.child.wait().ok();
            }
            Err(_) => {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }
}

impl Exit<'_> {
    pub fn status(&self) -> &ExitStatus {
        self.status
    }

    pub fn success(&self) -> bool {
        self.status.success()
    }

    /// The process this exit answers for — the one that was waited on, not a number remembered from it.
    pub fn pid(&self) -> u32 {
        self.supervisor.id()
    }
}

/// One member of a region whose process is proven gone: an arrival the host credited, joined to proof
/// that the reporting process is gone — the reaped exit of a child, or the `pidfd` the kernel delivered
/// beside a receipt for a writer nobody spawned.
///
/// This is the evidence [`crate::Session::recover`] asks for, and it is the reason that call is safe
/// where [`crate::Session::assume_dead`] is not: `assume_dead` takes an identity and asks the caller to
/// know the process behind it can never touch the region again, while a `Death` carries that knowledge
/// itself, assembled from two things the host answered for — who reported arriving, and which process is
/// gone.
///
/// Neither half proves the other, which is why the two entrances each bind their halves differently.
/// [`Death::witnessed`] compares a receipt's pid with a reaped child's because they arrive on two
/// channels — the report through `recvmsg`, the exit through `wait` — and only a shared pid says they name
/// one process.
#[cfg_attr(
    target_os = "linux",
    doc = " [`Death::credited`] gets its `Arrival` and `Writer` from the same `recvmsg`, so it does not \
           re-run that comparison; it checks the two things that can still fail once a caller holds the \
           halves apart: that the reference has actually gone, and that it names the same process the \
           report was credited to."
)]
#[cfg_attr(
    not(target_os = "linux"),
    doc = " The other entrance, `Death::credited`, is Linux's own: it joins a report to the `pidfd` the \
           kernel wrote beside it, so it exists only where the kernel will hand over such a reference, and \
           on this host a `Death` is assembled the one way above."
)]
/// What neither lets a caller do is supply the halves as numbers — there is no `Death::from_parts`, and
/// the reference type has no public constructor — because a receipt read from one process joined to a
/// reference for another would hand recovery authority over a member whose process is still standing, the
/// one failure this type exists to make unrepresentable.
///
/// ```compile_fail
/// use evering::layout::RegionId;
/// use evering::mapping::Peer;
/// use evering::process::Death;
///
/// fn invented(region: RegionId, member: Peer) -> Death {
///     Death::from_parts(region, member, 42)
/// }
/// ```
#[cfg(all(feature = "process", any(unix, windows)))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Death {
    arrival: Arrival,
}

#[cfg(all(feature = "process", any(unix, windows)))]
impl Death {
    /// Join a reported arrival to a reaped exit, or refuse them for naming different processes.
    ///
    /// The comparison is the whole of what this checks: `exit` is this process's own proof that a
    /// process is gone, `arrival` is the host's proof that a process said which member it took, and the
    /// two only add up when they are the same process. An arrival read before the child was waited on
    /// is the ordinary order — the status is what makes the death durable — and a child that died
    /// without reporting has no arrival to compose, so it stays a `Seat` to give back or an
    /// `assume_dead` to take on the caller's own word.
    pub fn witnessed(arrival: &Arrival, exit: &Exit<'_>) -> io::Result<Self> {
        if arrival.pid != exit.pid() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the arrival was reported by a process other than the one that was reaped",
            ));
        }
        Ok(Self { arrival: *arrival })
    }

    /// Join a kernel-credited arrival to the reference the kernel delivered for the process that wrote it.
    ///
    /// This is the entrance for a member that was introduced by meeting rather than launched, so there is
    /// no `&Supervisor` and no `wait` here: [`crate::os::Attendance::introduction`] reads the receipt on a
    /// rendezvous armed with `SO_PASSPIDFD`, and the kernel answers *in the same message* — the writer's
    /// report and the writer's `pidfd` together. Because they leave one `recvmsg` and the kernel writes both
    /// halves, there is no second channel whose pid could disagree, so this deliberately does not repeat
    /// `witnessed`'s comparison. What it does require is the two facts that survive once a caller holds an
    /// `Arrival` and a `Writer` as separate public arguments — that the process the `Writer` names is gone
    /// (`writer.gone()`), and that it is the process the report was credited to (`writer.pid()` against
    /// `arrival.pid`). Without them, the seam's forbidden outcome — recovery authority over a member whose
    /// process still stands — would become reachable by pairing one introduction's `Writer` with another's
    /// `Arrival`. This is answered after the writer exits and before it is reaped: `writer.pid()` reads the
    /// number the kernel still records for an unreaped process, so a death already collected by another route
    /// is [`Death::witnessed`]'s to build, not this one's.
    #[cfg(target_os = "linux")]
    pub fn credited(arrival: &Arrival, writer: &Writer) -> io::Result<Self> {
        if !writer.gone()? {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "the process this writer names has not exited",
            ));
        }
        if writer.pid()? != arrival.pid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the arrival was reported by a process other than the one the kernel credited",
            ));
        }
        Ok(Self { arrival: *arrival })
    }

    /// The region whose roster holds the member this death releases.
    pub fn region(&self) -> RegionId {
        self.arrival.region
    }

    /// The exact slot and generation whose process this death proves gone.
    pub fn peer(&self) -> Peer {
        self.arrival.member
    }

    /// The process that both reported the arrival and was reaped.
    pub fn pid(&self) -> u32 {
        self.arrival.pid
    }
}

impl Exited {
    /// The descriptor the host gave as this child's witness, absent when the death is already this
    /// process's own record. The waiting lane parks on it; nothing here reads it, and a witness that
    /// has none has nothing left to wait for.
    #[cfg(target_os = "linux")]
    pub(crate) fn descriptor(&self) -> Option<&Arc<OwnedFd>> {
        match &self.witness {
            Witness::Process(fd) => Some(fd),
            Witness::Recorded => None,
        }
    }
}

/// The writer of a receipt, as the kernel named it at delivery: a `pidfd` it handed over beside the
/// message, not a number this process chose with `pidfd_open`.
///
/// This is `Death`'s second acquisition route. [`Supervisor::exited`] can only witness a process this
/// supervisor spawned, and a bare `pidfd_open` names a process by a number a live process may already hold
/// and may be reused for; a `Writer` arrives already bound to whoever the kernel credited with the receipt,
/// so it can witness a member that was introduced by meeting rather than by being launched. It has no
/// public constructor — only `os::unix` builds one, from the `SCM_PIDFD` ancillary descriptor the kernel
/// wrote — because every argument [`Death::credited`] declines to compare rests on a `Writer` being an
/// unforgeable reference the kernel installed, not one a caller pointed at a process of its choosing.
///
/// ```compile_fail
/// use std::os::fd::OwnedFd;
/// use evering::process::Writer;
///
/// // A `Writer` comes only from the kernel, beside a receipt; a descriptor cannot be promoted into one.
/// fn forged(fd: OwnedFd) -> Writer {
///     Writer::received(fd)
/// }
/// ```
#[cfg(target_os = "linux")]
pub struct Writer(Arc<OwnedFd>);

#[cfg(target_os = "linux")]
impl Writer {
    /// Wrap the `pidfd` the kernel delivered as a receipt's writer. The only caller is `os::unix`'s receipt
    /// read, which reaches it out of the `SCM_PIDFD` cmsg; nothing else in this process holds it, so it is
    /// taken as owned here.
    pub(crate) fn received(fd: OwnedFd) -> Self {
        Self(Arc::new(fd))
    }

    /// Whether the process this reference names has exited, without consuming the readiness.
    ///
    /// A `pidfd` turns readable the instant its process is gone and stays readable after that, so `poll`ing
    /// it with no timeout answers the question a second time as well as the first — which is what lets the
    /// same `Writer` both prove a death to [`Death::credited`] and park a task on [`Writer::witness`].
    /// Nothing drains the descriptor; readiness is the whole report.
    pub fn gone(&self) -> io::Result<bool> {
        let mut watched = [nix::poll::PollFd::new(
            self.0.as_fd(),
            nix::poll::PollFlags::POLLIN,
        )];
        match nix::poll::poll(&mut watched, nix::poll::PollTimeout::from(0u16)) {
            Ok(_) => Ok(watched[0]
                .revents()
                .is_some_and(|flags| flags.intersects(nix::poll::PollFlags::POLLIN))),
            // An interrupted park is not an answer about the process; it is retried by the next call.
            Err(nix::errno::Errno::EINTR) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    /// The process number the kernel recorded for this reference, read from its `fdinfo` entry.
    ///
    /// A `Writer` is not trusted for this field — the reference itself is what carries the authority — but it
    /// lets the same death be shown `PartialEq` to one `witnessed` from a reaped child, and the number is one
    /// the kernel chose at delivery rather than looked up after the fact. It is recorded only while the writer
    /// is unreaped: the `fdinfo` entry keeps naming the process from its exit up to the moment somebody
    /// collects it, and drops the number afterward, which is why `Death::credited` is a check on a death that
    /// has happened and not yet been reaped — a death already reaped is `Death::witnessed`'s to compose.
    pub fn pid(&self) -> io::Result<u32> {
        use std::io::Read;
        let path = format!("/proc/self/fdinfo/{}", self.0.as_raw_fd());
        let mut text = String::new();
        std::fs::File::open(&path)?.read_to_string(&mut text)?;
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("Pid:") {
                return rest.trim().parse::<u32>().map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "the pidfd's fdinfo entry names no process",
                    )
                });
            }
        }
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no Pid field in the pidfd's fdinfo entry",
        ))
    }

    /// Hand this reference to the waiting lane, which parks a task until the writer is gone.
    ///
    /// [`crate::runtime`] already waits on exactly this descriptor shape — the `pidfd` `Supervisor::exited`
    /// builds — so an introduced member's death is waited on with no new machinery there.
    pub fn witness(&self) -> Exited {
        Exited {
            witness: Witness::Process(self.0.clone()),
        }
    }
}

/// The kernel's own reference to one process that this supervisor has not reaped yet.
///
/// `pidfd_open` has no wrapper in the `nix` features this crate enables, so this is the syscall and the
/// descriptor it returns, owned here. `0` asks for no flags — not even `PIDFD_NONBLOCK`, because
/// nothing reads the descriptor: it is parked on, never drained.
#[cfg(target_os = "linux")]
fn watch(pid: u32) -> io::Result<Exited> {
    // SAFETY: `pidfd_open` takes a pid and a flag word and, on success, hands back a fresh descriptor
    // that nothing else in this process refers to, which is what `OwnedFd` undertakes to close.
    let raw = unsafe { nix::libc::syscall(nix::libc::SYS_pidfd_open, pid, 0u32) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call succeeded, so `raw` is a descriptor this process owns and does not yet hold,
    // and a file descriptor fits the host's `c_int`.
    let fd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    Ok(Exited {
        witness: Witness::Process(Arc::new(fd)),
    })
}

/// The other arms refuse for lack of a mechanism written here, not for lack of one to write: a Windows
/// process handle is waitable, and `Supervisor::handle()` already returns it. Leaving that unwritten and
/// saying so is a policy statement — a caller that expects to park on a death and instead gets an error
/// can still fall back to `try_wait`, while a witness that never wakes strands it with no answer at all.
#[cfg(not(target_os = "linux"))]
fn watch(_: u32) -> io::Result<Exited> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no process-death witness is written for this target, and the child's end is reached through \
         `try_wait` or `wait`",
    ))
}
