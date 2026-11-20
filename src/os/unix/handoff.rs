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

use std::{
    env, io,
    os::fd::{AsFd, AsRawFd, RawFd},
    os::unix::process::CommandExt,
    process::Command,
    time::Instant,
};

use nix::unistd::{getppid, getuid};

use crate::{
    os::{
        Event, Peer, ReceivedResources, Ring,
        unix::{UnixFd, process::Socket},
    },
    process::{Bootstrap, Introduction, RENDEZVOUS, SEAT, Supervisor},
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
    pub fn send<F: AsFd>(
        self,
        _child: &Supervisor,
        _deadline: Instant,
        bootstrap: &Bootstrap,
        mapping: &UnixFd<F>,
        event: &Event,
        ring: &Ring,
    ) -> io::Result<()> {
        self.socket
            .send(bootstrap, &[mapping.as_fd(), event.as_fd(), ring.as_fd()])
    }

    /// Takes the handover this process was granted, and attests the peer before anything is read.
    ///
    /// The order is the whole point of the call: on a connected pair the kernel's answer is available
    /// as soon as the descriptor is, so a process that merely announces a number, or owns a socket it
    /// made itself, is refused without ever blocking on a message it was never sent — and without any
    /// descriptor being installed in its table first.
    pub fn receive() -> io::Result<(Peer, ReceivedResources)> {
        let socket = granted_socket()?;
        let peer = attested(&socket)?;
        let promised = announced_seat()?;
        let (bootstrap, resources) = socket.recv(3)?.into_parts();
        drop(socket);
        let mut resources = resources.into_vec();
        Ok((
            peer,
            ReceivedResources {
                bootstrap,
                mapping: UnixFd::from_fd(resources.remove(0)).map_err(io::Error::from)?,
                event: Event::from_owned_fd(resources.remove(0)),
                ring: Ring::from_owned_fd(resources.remove(0)),
                seat: promised,
            },
        ))
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
/// Read after the attestation, not before it: the environment is this process's own, so the kernel has
/// nothing to say about it and nothing here can be forged into a handover. A slot that is announced in
/// a shape no slot has is refused as the malformed text it is rather than arriving at whatever the
/// readable half of it names.
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

/// The kernel's own answer about the peer, checked against this process's parent.
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
