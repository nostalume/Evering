//! The Unix half of a handover: a connected socketpair is the meeting, and the single-use `Grant` is
//! one end of it, so admitting a process means inheriting a descriptor rather than publishing a name
//! somebody else could reach first. `bind` makes the pair, `Grant::admit` installs its end at
//! `INHERITED` in the child and consumes itself — and `Grant::promising` puts beside it the member slot
//! the child was told to arrive at, which goes out by the same single use, `Handoff::send` moves the
//! bootstrap plus the mapping, event and ring, and `Handoff::receive` attests the peer *before* adopting
//! anything — the order is the point, since on a pair the kernel's answer is available the moment the
//! descriptor is, so a process that merely announced a number is refused without blocking and without a
//! foreign object installed in its table. `shared` creates the memfd and keeps the access its caller
//! named as the object's own bound, because a memfd carries none and the record is the only place that
//! word can be answered for.
//!
//! Three obligations leave this file. The count `receive` passes to `Socket::recv` and the order it then
//! removes the three resources in are one contract with `super::process` and with every sender, and a
//! mismatch delivers one peer's event as another's mapping with nothing to complain about. The promised
//! slot travels outside that contract, through the environment, precisely so that adding it does not
//! move a descriptor by one place — and it is a claim this file neither makes nor vouches for, which
//! leaves the roster, not the reader, to decide whether it was ever promised. The adoption rule that
//! makes the attestation testable — classify by borrow, own only on the accepting path — lives in
//! `Socket::granted`, not here. And the non-Linux arm refuses for lack of a mechanism, which is a
//! policy statement rather than a placeholder: writing one for another host should not quietly turn an
//! unattested handover into a permitted one.
//!
//! The return leg is the receipt, and it changes what a meeting *ends* when: the granted descriptor is
//! now the child's way back, so `Attendance::arrival` is what reconciles the child's own word about the
//! member it took with the slot this file's promise named, and `Death` is only reachable through both.
//! What this leg decides and no test can undo is that three separate statements have to agree before an
//! arrival means anything — the kernel's answer about who wrote, the identity the writer reported, and
//! the identity the grant promised. Reading only the first two would let a child that arrived in a
//! stranger's slot hand the parent authority to reap it.
//!
//! What the receipt cannot say is which member a *death* reached for a process this file never promised:
//! no promise, no comparison, and so no arrival — an unpromised child that joined as a member of its own
//! leaves its parent exactly where it was, with [`assume_dead`](crate::Session::assume_dead) and its own
//! word as the only evidence. That is a boundary of the design, not a gap in this file.

use std::{
    env, io,
    os::fd::{AsFd, AsRawFd, RawFd},
    os::unix::process::CommandExt,
    process::Command,
    time::Instant,
};

use nix::unistd::{getppid, getuid};

use crate::{
    layout::RegionId,
    os::{
        Event, Peer, ReceivedResources, Ring,
        unix::{UnixFd, process::Socket},
    },
    process::{Arrival, Bootstrap, Introduction, RENDEZVOUS, SEAT, Supervisor},
};

pub type Shared = UnixFd<std::os::fd::OwnedFd>;

/// The descriptor number a granted rendezvous is installed at in the process it is granted to.
/// Three keeps it clear of the standard handles, which are the only descriptors `Command` itself
/// redirects, and the number is announced through the environment because the number is not the
/// authority: the descriptor behind it is.
const INHERITED: RawFd = 3;

/// Creates the shared object a peer receives as a descriptor.
///
/// `name` labels the object for the host's own inspection. `access` is kept as the object's own
/// bound, not applied to it: a memfd carries no per-object permission, so the protection a process
/// gets is still decided at map time, where `Access` becomes the mapping's `ProtFlags`. What the
/// bound changes is that this crate now refuses a later `Request` wider than the word the object was
/// made under, which is the half of the promise no host can keep here.
///
/// The bound is the maker's, and it does not cross the socket: a peer that receives this object as a
/// descriptor is handed bytes and no claim, so its own mappings are answered for by what the kernel
/// grants it rather than by what its parent asked for.
pub fn shared(name: &str, size: usize, access: crate::mapping::Access) -> io::Result<Shared> {
    UnixFd::memfd(name, size, false)
        .map(|object| object.bound_to(access))
        .map_err(io::Error::from)
}

pub struct Handoff {
    socket: Socket,
}

/// The capability to admit exactly one process to one meeting.
///
/// There is nothing here to read, copy, or hand over a command line: the rendezvous is a pair of
/// connected descriptors, and admitting a process means giving it one of them at `exec`. A name
/// that can be looked up can be claimed by whoever looks it up first; a descriptor that has to be
/// inherited cannot, which is why `Grant` replaced the address string.
///
/// The `promised` half is a different kind of thing: not a capability but a message, the member slot
/// the holder of a [`Seat`](crate::Seat) decided this process should arrive at. Carrying it here
/// means admitting a process and telling it who it is cannot be done in the wrong order or to the
/// wrong child, because both go out through the one `admit` that consumes the grant.
pub struct Grant {
    socket: Socket,
    promised: Option<Introduction>,
}

impl Handoff {
    /// Creates one meeting and the single-use grant that admits a participant to it.
    pub fn bind() -> io::Result<(Self, Grant)> {
        let (here, there) = Socket::pair()?;
        here.credit_writers()?;
        Ok((
            Self { socket: here },
            Grant {
                socket: there,
                promised: None,
            },
        ))
    }

    /// Moves the region, the doorbell and the ring to the process the grant was admitted to.
    ///
    /// `child` and `deadline` carry no weight on this platform, and the reason is worth stating:
    /// the rendezvous is already connected when `bind` returns, so there is nothing to wait for and
    /// no peer identity the kernel will report here that is not this process's own — a socketpair
    /// reports its creator at both ends. What binds the handover to `child` is that only a process
    /// `child` was `exec`d from can hold the granted descriptor, and the receiver attests on the
    /// other side of it. Windows, where the parent listens, checks `child` and needs `deadline`.
    ///
    /// The rendezvous is not spent by the offer: what `send` hands back is the same end, now read for
    /// the receipt the admitted process writes down that end. Keeping it is the only way the parent
    /// learns which member its promise filled, and dropping it is what tells the child that nobody is
    /// taking attendance any more.
    pub fn send<F: AsFd>(
        self,
        _child: &Supervisor,
        _deadline: Instant,
        bootstrap: &Bootstrap,
        mapping: &UnixFd<F>,
        event: &Event,
        ring: &Ring,
    ) -> io::Result<Attendance> {
        let socket = self.socket;
        socket.send(bootstrap, &[mapping.as_fd(), event.as_fd(), ring.as_fd()])?;
        Ok(Attendance { socket })
    }

    /// Takes the handover this process was granted, and attests the peer before anything is read.
    ///
    /// The order is the whole point of the call: on a connected pair the kernel's answer is available
    /// as soon as the descriptor is, so a process that merely announces a number, or owns a socket it
    /// made itself, is refused without ever blocking on a message it was never sent — and without any
    /// descriptor being installed in its table first.
    ///
    /// The granted end is kept rather than closed, because it is the one channel back to the process
    /// that admitted this one: the [`Receipt`] it returns is how this process says which member it
    /// became.
    ///
    /// The announced slot is read after the offer, not before it. A child that finds its announcement
    /// unreadable still declines a meeting it was admitted to, and the handover its parent is already
    /// writing has to reach somebody: a reader that left first makes the parent's
    /// [`Handoff::send`](Handoff::send) answer with whatever the host reports for a vanished peer, which
    /// is the one outcome the two arms cannot be allowed to disagree about.
    pub fn receive() -> io::Result<(Peer, ReceivedResources, Receipt)> {
        let socket = granted_socket()?;
        let peer = attested(&socket)?;
        let (bootstrap, resources) = socket.recv(3)?.into_parts();
        let mut resources = resources.into_vec();
        let promised = announced_seat()?;
        Ok((
            peer,
            ReceivedResources {
                bootstrap,
                mapping: UnixFd::from_fd(resources.remove(0)).map_err(io::Error::from)?,
                event: Event::from_owned_fd(resources.remove(0)),
                ring: Ring::from_owned_fd(resources.remove(0)),
                seat: promised,
            },
            Receipt { socket },
        ))
    }
}

/// The parent's end of a meeting it has held, which is where an admitted process reports what it became.
///
/// [`Handoff::send`] is the only way to obtain one, so there is no reading of an arrival before the
/// handover it answers. What it answers for is narrower than the offer did: one receipt, of the one
/// process the consumed [`Grant`] admitted, against the one identity that grant promised.
pub struct Attendance {
    socket: Socket,
}

/// The admitted process's end of the rendezvous, which is how it reports the member it arrived as.
///
/// [`Handoff::receive`] keeps the granted descriptor open rather than closing it, and this is the whole
/// reason: a receipt is only worth having if it arrives by a channel the writer cannot choose, and the
/// end this process was handed is the only one it holds that the kernel will credit to nobody else.
pub struct Receipt {
    socket: Socket,
}

impl Attendance {
    /// Reads the receipt the promised process wrote, and refuses one that says anything else.
    ///
    /// `promised` is not a courtesy: the receipt is the child's own word about which member it took, and
    /// a child can take a different one than it was told. Compared against the promise, the two answers
    /// either agree — which is the record that the promised slot was *stood in*, not merely left open —
    /// or they do not, and then this process has been told something it did not ask about and nothing is
    /// bound to anything. [`Death`](crate::process::Death) cannot be built from a refusal, and that is
    /// the point: an unreconciled promise stays an [`assume_dead`](crate::Session::assume_dead) decision
    /// or a [`Seat`](crate::Seat) to give back.
    ///
    /// `deadline` bounds the read, and on this endpoint it is the only bound there is: a datagram rendezvous
    /// answers a peer's departure with no readiness and no hangup, so `TimedOut` covers a child that is slow
    /// exactly as it covers a child that met, said nothing and exited. `UnexpectedEof` is the answer a
    /// seqpacket rendezvous gives, which the host does report as an end. Which is also why a parent that has
    /// to know its child is gone asks the child and not the socket: [`Supervisor::wait`] reaps, and a
    /// [`Death`](crate::process::Death) is that answer beside an arrival rather than either alone.
    pub fn arrival(
        self,
        child: &Supervisor,
        promised: Introduction,
        deadline: Instant,
    ) -> io::Result<Arrival> {
        credited(&self.socket, child, promised, deadline)
    }
}

impl Receipt {
    /// Reports the member this process holds to the process that admitted it.
    ///
    /// The two parts come from this process's own `Session` — the region it mapped and the identity the
    /// roster gave it — and not from the announcement in its environment: reporting what it was *told*
    /// would echo the parent's promise back at it and prove nothing, while reporting what it *became* is
    /// the one thing the parent cannot establish alone. What the child writes is not trusted for its
    /// content either way; the promise is what decides whether the report is news.
    pub fn arrived(&self, region: RegionId, member: crate::mapping::Peer) -> io::Result<()> {
        self.socket
            .report(&Introduction::new(region, member).announcement())
    }
}

impl Grant {
    /// Name the member slot the process this grant admits is to arrive at.
    ///
    /// Nothing here is a capability: a slot the admitting session does not hold promised is refused by
    /// the roster when the child arrives, so the promise has to be made with
    /// [`Session::reserve_seat`](crate::Session::reserve_seat) and kept — by holding the `Seat` — until
    /// that arrival or until it is given back.
    pub fn promising(mut self, seat: Introduction) -> Self {
        self.promised = Some(seat);
        self
    }

    /// Installs this rendezvous in `command`, so that the process it spawns holds the granted end.
    ///
    /// `dup2` is the whole mechanism and one of its details is load-bearing: both ends of the pair this
    /// module makes are close-on-exec, and `dup2` clears that flag on the descriptor it writes, which is
    /// what lets the granted end survive the `exec` that every other descriptor here does not. A rewrite
    /// that inherited the number instead — or that re-set the flag afterwards — would hand the child a
    /// closed descriptor and the failure would surface as a rendezvous that announces nothing.
    ///
    /// Consuming `self` is what makes the grant single-use: after this call the sender has no way to
    /// point a second child at the same meeting, and no name left for anyone else to claim first. It
    /// also means a promised slot is announced to exactly one child, because the second child would
    /// have to be admitted by a grant that no longer exists.
    pub fn admit(self, command: &mut Command) -> io::Result<()> {
        command.env(RENDEZVOUS, INHERITED.to_string());
        if let Some(promised) = &self.promised {
            command.env(SEAT, promised.announcement());
        }
        let granted = self.socket;
        // SAFETY: the hook runs between fork and exec in the child, and calls only `dup2`, which is
        // async-signal-safe. `granted` is moved into it so the descriptor stays open until the
        // command is spawned, and is closed with the command if it never is.
        unsafe {
            command.pre_exec(move || {
                let here = granted.as_fd().as_raw_fd();
                if nix::libc::dup2(here, INHERITED) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Ok(())
    }
}

fn refused(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, reason)
}

/// Adopts the descriptor the grant installed, if the claim in the environment checks out.
fn granted_socket() -> io::Result<Socket> {
    let announced = env::var(RENDEZVOUS)
        .map_err(|_| refused("no evering rendezvous was granted to this process"))?;
    let raw: RawFd = announced
        .parse()
        .map_err(|_| refused("the announced rendezvous is not a descriptor number"))?;
    if raw < 0 {
        return Err(refused(
            "the announced rendezvous is not a descriptor number",
        ));
    }
    // SAFETY: the number is a claim from the environment, not a descriptor this module owns, so it
    // is classified through a borrow and taken over only once it is known to be a rendezvous.
    unsafe { Socket::granted(raw) }
}

/// The member slot this process was told to arrive at, if its parent announced one.
///
/// Read after the attestation and after the offer, never before either: the environment is this
/// process's own, so the kernel has nothing to say about it and nothing here can be forged into a
/// handover. A slot that is announced in a shape no slot has is refused as the malformed text it is
/// rather than arriving at whatever the readable half of it names.
fn announced_seat() -> io::Result<Option<Introduction>> {
    let announced = match env::var(SEAT) {
        Ok(announced) => announced,
        Err(env::VarError::NotPresent) => return Ok(None),
        Err(_) => return Err(refused("the announced member slot is not text")),
    };
    Introduction::announced(&announced)
        .map(Some)
        .ok_or_else(|| refused("the announced member slot names no slot at all"))
}

/// The kernel's own answer about the process that owns the peer end, checked against this process's parent.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn attested(socket: &Socket) -> io::Result<Peer> {
    let credentials = socket.peer_credentials()?;
    let parent = getppid().as_raw() as u32;
    if credentials.pid != parent || credentials.uid != getuid().as_raw() {
        return Err(refused(
            "the peer behind the granted rendezvous is not this process's parent",
        ));
    }
    Ok(Peer::introduced(credentials.pid))
}

/// No other unix variant is refused for lack of a mechanism name, but for lack of one that has been
/// written and run here: an unattested handover completes nowhere unless a policy says so out loud,
/// which is card 3.4's `Agreement` clause and is not implemented by this file.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn attested(_socket: &Socket) -> io::Result<Peer> {
    Err(refused(
        "this platform has no peer attestation implemented, and an unattested handover is refused",
    ))
}

/// The receipt this rendezvous carries, credited to the admitted process and reconciled with the promise.
///
/// Three checks, each answering a different way the two halves could fail to be about the same thing. The
/// kernel's pid and uid, because a report from anybody else says nothing about this child. The promise,
/// because a report of a *different* member is a child that arrived somewhere else and a `Death` built
/// from it would reap whoever does stand there. And the frame's own grammar, refused by `Socket::arrival`
/// before any of this sees the text, so a scribbled receipt is never compared with anything.
///
/// The pid arrives with the message rather than before it, which is the one difference from `attested`: a
/// receipt is credited at the read, and there is nothing in it to adopt — no descriptor, no object, no
/// state this process changes — so nothing is installed ahead of the answer about who wrote it.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn credited(
    socket: &Socket,
    child: &Supervisor,
    promised: Introduction,
    deadline: Instant,
) -> io::Result<Arrival> {
    let (arrived, credentials) = socket.arrival(deadline)?;
    if credentials.pid != child.id() || credentials.uid != getuid().as_raw() {
        return Err(refused(
            "the receipt on this rendezvous was not written by the process it was promised to",
        ));
    }
    if arrived != promised {
        return Err(refused(
            "the arrival reported is not the member slot that was promised",
        ));
    }
    Ok(Arrival::reported(
        arrived.region(),
        arrived.peer(),
        credentials.pid,
    ))
}

/// The arms that credit no writer get the same answer `attested` gives them, for the same reason: an
/// arrival nobody can attribute is not evidence of anything, and a value a caller would hand to
/// `Session::recover` would be exactly the unattested handover this file refuses to complete.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn credited(
    _socket: &Socket,
    _child: &Supervisor,
    _promised: Introduction,
    _deadline: Instant,
) -> io::Result<Arrival> {
    Err(refused(
        "this platform credits no writer, and an uncredited arrival is refused",
    ))
}
