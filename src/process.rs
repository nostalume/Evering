//! Owns the frozen ceilings a handoff is measured against — `MAX_BOOTSTRAP`, `MAX_RESOURCES`, the
//! magic a rendezvous carries and the keys a child is told things through — and the linear ownership of
//! exactly one child. Bytes enter only through `Bootstrap::new`, and a promised member slot has exactly
//! two entrances: the `Seat` of the process holding the promise, and the environment text of the
//! process it was promised to, which is what keeps an `Introduction` from being a number a caller
//! typed. A child's terminal state is reached only through `try_wait`, `wait` or `kill_wait`, and the
//! status is then memoized, which is what makes an `Exit` a proof rather than another poll.
//!
//! `Drop` has to reap a child nobody waited on. The suite covers waiting, killing and a spawn that
//! failed, but nothing drops an un-waited supervisor, so taking that arm out leaks a live child with no
//! failing test. `MAX_RESOURCES` is enforced on the receiving side, not here.
use core::marker::PhantomData;
use std::{
    io,
    process::{Child, Command, ExitStatus},
};

use crate::{mem::Peer, schema::RegionId};

pub const MAX_BOOTSTRAP: usize = 4096;
pub const MAX_RESOURCES: usize = 8;
pub(crate) const HANDOFF_MAGIC: [u8; 4] = *b"EVR1";

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

#[cfg(test)]
#[test]
fn handoff_version_is_frozen() {
    assert_eq!(HANDOFF_MAGIC, *b"EVR1");
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
pub struct Exit<'a> {
    status: &'a ExitStatus,
    _supervisor: PhantomData<&'a Supervisor>,
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

    pub fn try_wait(&mut self) -> io::Result<Option<Exit<'_>>> {
        if self.status.is_none() {
            self.status = self.child.try_wait()?;
        }
        Ok(self.status.as_ref().map(|status| Exit {
            status,
            _supervisor: PhantomData,
        }))
    }

    pub fn wait(&mut self) -> io::Result<Exit<'_>> {
        if self.status.is_none() {
            self.status = Some(self.child.wait()?);
        }
        Ok(Exit {
            status: self.status.as_ref().unwrap(),
            _supervisor: PhantomData,
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
                                    _supervisor: PhantomData,
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
            _supervisor: PhantomData,
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
}
