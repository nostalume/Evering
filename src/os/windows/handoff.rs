//! The Windows half of a handover: a named-pipe listener is the meeting, and the grant is the pipe's
//! address plus this process's own pid, both — with the member slot `Grant::promising` named — installed
//! in the admitted child's environment by `admit`, which consumes the grant. `send` accepts the client
//! and refuses anything that is not the `child` it was given, bounded by `deadline` because the pipe
//! instance exists long before the child runs; `receive` connects, asks the kernel whose pipe this is and
//! refuses unless that answer matches the announced introducer, and only then reads the offer. `shared`
//! is where a section's protection is fixed, since that is the earliest point a Windows object can be
//! restricted.
//!
//! Both legs are load-bearing in a way the Unix arm's are not: the address is discoverable by any
//! process on the host while the pipe exists, so nothing but `accept`'s check and `server_pid`'s
//! protects the offer from a claimant that merely read it — drop either and the impostor test that
//! covers the other direction would still pass. What the two arms must keep agreeing on is not here:
//! `recv(3)`'s count and the order the three handles are removed in are the same positional contract the
//! Unix leg implements, and so are the promised slot's text and the fact that both legs read it only
//! after the kernel has answered and the offer has been taken, which is what keeps one arm's child able
//! to meet the other arm's parent. And `Grant` staying single-use is what stops one address admitting
//! two children. The offer leg has since been executed on this host, by `tests/seat_announcement.rs`:
//! bind, admit, send, the introducer check in `receive`, and the promised slot read both ways. So has the
//! return leg the next paragraph is about, by `tests/arrival_receipt_windows.rs`: the report, the client
//! named again at the read, and the comparison against the promise.
//!
//! The return leg is the receipt, and it is where the two arms' mechanisms differ most even though their
//! answers are the same shape. Here the credit is the instance: `accept` proved whose client connected,
//! one instance serves one client, and `client_pid` re-asks at the read so the arrival is attributed to
//! the process the caller is about to reap rather than to the pipe. On Unix the credit is a field the
//! kernel writes beside the message. An arrival that means one thing means the other only if this leg's
//! `nMaxInstances` stays `1` — widened, the report would come from whichever client connected, and the
//! pid here would name a process that never held the promise. Widening it is an obligation no case here
//! touches, and what the leg has now shown on this host is the half the paragraph rests on: after the
//! client has been reaped the instance still names it, and what it wrote is still queued, so a caller that
//! takes the exit first and reads the arrival second is answering for one process either way.

use std::{env, io, os::windows::io::AsHandle, process::Command, time::Instant};

use crate::{
    layout::RegionId,
    os::{
        Event, Provenance, ReceivedResources, Ring,
        windows::{
            Section,
            process::{Listener, Socket},
        },
    },
    process::{
        Arrival, Bootstrap, INTRODUCER, Introduction, RENDEZVOUS, SEAT, Supervisor, TRUST,
        announced_trust,
    },
};

pub type Shared = Section<std::os::windows::io::OwnedHandle>;

/// Creates the shared object a peer receives as a handle.
///
/// `access` selects the section's protection at creation, which is the earliest point a
/// Windows section can be restricted. `name` is unused: the section is anonymous and moves
/// only as a handed-over handle, so there is no namespace entry a caller could name or
/// collide with.
pub fn shared(_name: &str, size: usize, access: crate::mapping::Access) -> io::Result<Shared> {
    Section::anonymous(size, access)
}

pub struct Handoff {
    listener: Listener,
}

/// The capability to admit exactly one process to one meeting.
///
/// A Windows named pipe has to be reached by name, so this holds the one the listener created and
/// installs it in the admitted process's environment. What the API no longer offers is the name as a
/// `&str` a caller can pass around: the peer check in `send` and the one in `receive` are what decide
/// who the meeting is with, and both of them read the kernel's answer rather than a claim.
///
/// The `trusts` half is a policy and this is the only place one can be written, for the same reason the
/// promised slot rides along: what the introducer did not say, the receiver is not allowed to decide.
pub struct Grant {
    address: String,
    introducer: u32,
    promised: Option<Introduction>,
    trusts: bool,
}

fn refused(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, reason)
}

impl Handoff {
    /// Creates one meeting and the single-use grant that admits a participant to it.
    pub fn bind() -> io::Result<(Self, Grant)> {
        let listener = Listener::bind()?;
        let address = listener.name().to_owned();
        Ok((
            Self { listener },
            Grant {
                address,
                introducer: std::process::id(),
                promised: None,
                trusts: false,
            },
        ))
    }

    /// Moves the region, the doorbell and the ring to the process the grant was admitted to.
    ///
    /// `accept` asks the kernel whose client connected and refuses anything that is not `child`, and
    /// `deadline` bounds the wait for that client to appear — the pipe instance exists long before
    /// the child has run.
    ///
    /// What `accept` reached is kept rather than closed: the instance is the only channel this process
    /// has back to the child it admitted, and one instance serves one client, so the same handle that
    /// carried the offer is the one that takes the receipt.
    pub fn send<F: AsHandle>(
        self,
        child: &Supervisor,
        deadline: Instant,
        bootstrap: &Bootstrap,
        mapping: &Section<F>,
        event: &Event,
        ring: &Ring,
    ) -> io::Result<Attendance> {
        let socket = self.listener.accept(child, deadline)?;
        socket.send(
            child,
            bootstrap,
            &[mapping.as_handle(), event.as_handle(), ring.as_handle()],
        )?;
        Ok(Attendance { socket })
    }

    /// Takes the handover this process was granted, and attests the peer before any of it is
    /// released to the caller.
    ///
    /// The two ways the kernel's answer can fail to arrive are not the same failure, and the grant's
    /// policy speaks to only one of them. An answer that names somebody other than the announced
    /// introducer is the impostor that read the pipe address off a parent's environment, and it is
    /// refused under every policy: the introducer cannot vouch for a server it did not open. No answer
    /// at all is a meeting whose other end exists — the connect succeeded, so the rendezvous is
    /// established as one this process was given — and which was not asked, and that is the one a
    /// trusting introduction carries, as `Provenance::Unavailable`.
    ///
    /// The client end is kept, because it is the way back: the [`Receipt`] this returns is how the
    /// admitted process tells its parent which member it became.
    ///
    /// The announced slot is read after the offer for the same reason the Unix leg reads it there: a
    /// message-mode pipe write has no queue to outlive its reader, so a child that declined before
    /// taking the handover would report itself to its parent as a broken pipe.
    pub fn receive() -> io::Result<(Provenance, ReceivedResources, Receipt)> {
        let address = env::var(RENDEZVOUS)
            .map_err(|_| refused("no evering rendezvous was granted to this process"))?;
        let introducer: u32 = env::var(INTRODUCER)
            .map_err(|_| refused("the granted rendezvous names no introducer"))?
            .parse()
            .map_err(|_| refused("the announced introducer is not a process id"))?;
        let trusts = announced_trust()?;
        let socket = Socket::connect(&address)?;
        let provenance = match socket.server_pid() {
            Ok(server) if server == introducer => Provenance::Attested { pid: server },
            Ok(_) => {
                return Err(refused(
                    "the peer behind the granted rendezvous is not the process that granted it",
                ));
            }
            Err(_) => Provenance::unattested(Provenance::Unavailable, true, trusts)?,
        };
        let (bootstrap, resources) = socket.recv(3)?.into_parts();
        let mut resources = resources.into_vec();
        let promised = announced_seat()?;
        Ok((
            provenance,
            ReceivedResources {
                bootstrap,
                mapping: Section::from_owned_handle(resources.remove(0)),
                event: Event::from_owned_handle(resources.remove(0)),
                ring: Ring::from_owned_handle(resources.remove(0)),
                seat: promised,
            },
            Receipt { socket },
        ))
    }
}

/// The parent's end of the instance it accepted, which is where the admitted child reports what it became.
///
/// [`Handoff::send`] is the only way to obtain one: the instance exists only after the client behind it
/// was named and checked, so an arrival read here is about the process that passed that check and nobody
/// else.
pub struct Attendance {
    socket: Socket,
}

/// The admitted process's end of the pipe, which is how it reports the member it arrived as.
pub struct Receipt {
    socket: Socket,
}

impl Attendance {
    /// Reads the receipt, attributed to the client of this instance and reconciled with the promise.
    ///
    /// The order is the Windows arm's own, and it is the one the Unix arm cannot have: `accept` already
    /// proved whose instance this is, so the client is named *before* the message is read rather than
    /// arriving beside it. Both arms end with the same three answers — whose, what, promised-what — and
    /// both refuse a receipt that fails one of them, which is what lets a caller hand the result to
    /// [`Death`](crate::process::Death) without knowing which leg it came off.
    pub fn arrival(
        self,
        child: &Supervisor,
        promised: Introduction,
        deadline: Instant,
    ) -> io::Result<Arrival> {
        let client = self.socket.client_pid()?;
        if client != child.id() {
            return Err(refused(
                "the client of this rendezvous is not the process it was promised to",
            ));
        }
        let arrived = self.socket.arrival(deadline)?;
        if arrived != promised {
            return Err(refused(
                "the arrival reported is not the member slot that was promised",
            ));
        }
        Ok(Arrival::reported(arrived.region(), arrived.peer(), client))
    }
}

impl Receipt {
    /// Reports the member this process holds to the process that admitted it.
    ///
    /// The pair comes from this process's own session rather than from the announcement in its
    /// environment: echoing the promise would tell the parent nothing it did not already know, while the
    /// identity the roster actually gave this process is the record of who stood in the slot.
    pub fn arrived(&self, region: RegionId, member: crate::mapping::Peer) -> io::Result<()> {
        self.socket
            .report(&Introduction::new(region, member).announcement())
    }
}

/// The member slot this process was told to arrive at, if its parent announced one.
///
/// Read after the introducer check and after the offer, like the Unix leg reads it after its
/// attestation: the environment is this process's own, so nothing here can be forged into a handover,
/// and a slot announced in a shape no slot has is refused as the malformed text it is rather than
/// arrived at.
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

    /// Say to the process this grant admits that a peer it cannot attest may complete the handover.
    ///
    /// The word reaches one failure and not the other, on this platform: a server that answers and names
    /// somebody else is refused whatever the grant said, because that is the claimant that read the pipe
    /// address out of a parent's environment, and the introducer has no authority to vouch for a pipe it
    /// did not open. A server that was not asked at all is the handover this releases, and it arrives
    /// named `Provenance::Unavailable` rather than with a process id nobody confirmed.
    pub fn trusting_introduction(mut self) -> Self {
        self.trusts = true;
        self
    }

    /// Installs this rendezvous in `command`, so the process it spawns is the one meeting.
    ///
    /// Consuming `self` is what makes the grant single-use: a second `admit` cannot be written, and
    /// the address never becomes something a caller threads through arguments, where every process
    /// on the host could read it. It also means a promised slot is announced to exactly one child,
    /// because the second child would have to be admitted by a grant that no longer exists.
    ///
    /// Everything the introducer meant goes out here — the address, its own process id, the promised
    /// slot, and the policy word if [`trusting_introduction`](Grant::trusting_introduction) was called —
    /// so the receiver has four announcements to read and no decision of its own to make about any of
    /// them.
    pub fn admit(self, command: &mut Command) -> io::Result<()> {
        command.env(RENDEZVOUS, self.address.as_str());
        command.env(INTRODUCER, self.introducer.to_string());
        if let Some(promised) = &self.promised {
            command.env(SEAT, promised.announcement());
        }
        if self.trusts {
            command.env(TRUST, "1");
        }
        Ok(())
    }
}
