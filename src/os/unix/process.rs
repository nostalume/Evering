//! One local endpoint of a Unix descriptor-transfer channel, and the framing of a single all-or-nothing
//! offer: `HEADER` bytes of magic, bootstrap length and resource count, the bootstrap itself, then the
//! descriptors as `SCM_RIGHTS` ancillary data. `send` writes that shape and `recv` refuses anything that
//! does not answer it — truncated control, a short or long datagram, a declared count that differs from
//! the expected one or from what the kernel actually delivered — and `granted` adopts a descriptor
//! another process announced only after classifying it through a borrow. Nothing mutates shared state:
//! each `Offer` is owned by whoever received it.
//!
//! The header layout and the positional resource order are the handover wire format, matched on the far
//! side by `super::handoff` and frozen by the magic in `crate::process`, so widening a length field or
//! reordering what a count means is a format event here, not a local edit. Two more obligations have no
//! test in this file to catch them: every adopted descriptor is marked close-on-exec because a handover
//! that survives an `exec` leaks a peer's object into a third process, and `Credentials` has to keep
//! `pid`, because the attestation in `super::handoff` is a comparison of that field against `getppid()`
//! and a pair of `uid`/`gid` cannot carry the check.
//!
//! The second frame is the receipt: `ARRIVAL_MAGIC`, the arrival text, and no descriptors. It is read
//! under a bound the caller names rather than one left on the socket, because the endpoint is the same
//! one the offer crossed and a `SO_RCVTIMEO` would be a change to what the other end of the meeting
//! experiences — and because on a datagram pair the host answers a peer's departure with nothing at all, so
//! a bound is the only end a reader of this rendezvous can be given. What no test here catches is the
//! pairing of the two tags: a `read_receipt` that accepted
//! `EVR1` would report a handover the receiver never got, and a `recv` that accepted `EVA1` would take a
//! child's report as an offer and hand back three resources that were never sent.

use std::{
    io::{self, IoSlice, IoSliceMut},
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd},
        unix::net::UnixDatagram,
    },
    path::Path,
};

use nix::{
    cmsg_space,
    fcntl::{FcntlArg, FdFlag, fcntl},
    sys::socket::{
        ControlMessage, ControlMessageOwned, MsgFlags, SockType, getsockopt, recvmsg, sendmsg,
        sockopt,
    },
};

#[cfg(any(target_os = "linux", target_os = "android"))]
use std::time::Instant;

#[cfg(any(target_os = "linux", target_os = "android"))]
use nix::{
    errno::Errno,
    poll::{PollFd, PollFlags, PollTimeout, poll},
    sys::socket::{UnixCredentials, setsockopt},
};

use crate::process::{Bootstrap, MAX_BOOTSTRAP, MAX_RESOURCES};

#[cfg(any(target_os = "linux", target_os = "android"))]
use crate::process::Introduction;

const HEADER: usize = 8;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Whether the kernel says this descriptor can carry a rendezvous, asked before anything owns it.
///
/// One question, asked once, because two entrances have to get the same answer: the descriptor number a
/// grant announced in the environment, and a descriptor a host placed here by its own means. A datagram
/// or seqpacket endpoint is taken over as a meeting; anything else is refused without a byte of it being
/// read, and the refusal says so in the same words whichever way it arrived.
fn is_rendezvous(announced: &BorrowedFd<'_>) -> io::Result<()> {
    let kind = getsockopt(announced, sockopt::SockType).map_err(io::Error::from)?;
    if matches!(kind, SockType::Datagram | SockType::SeqPacket) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the descriptor offered as a rendezvous is not a datagram endpoint",
        ))
    }
}

/// What the kernel recorded about the process that owns the peer end of a socket.
///
/// The identifiers are the kernel's own, captured when the two ends were connected, so neither side
/// of the pair can assert or revoke them. `pid` is the kernel's signed process number widened to the
/// unsigned form every other identity in this crate uses.
#[cfg(any(target_os = "linux", target_os = "android"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Credentials {
    pub pid: u32,
    pub uid: u32,
    pub gid: u32,
}

/// One local endpoint of a Unix descriptor-transfer channel.
#[derive(Debug)]
pub struct Socket(UnixDatagram);

/// One all-or-nothing received resource set.
#[derive(Debug)]
pub struct Offer {
    bootstrap: Bootstrap,
    resources: Box<[OwnedFd]>,
}

impl Socket {
    pub fn pair() -> io::Result<(Self, Self)> {
        let (left, right) = UnixDatagram::pair()?;
        Ok((Self(left), Self(right)))
    }

    pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
        UnixDatagram::bind(path).map(Self)
    }

    pub fn connect(path: impl AsRef<Path>) -> io::Result<Self> {
        let socket = UnixDatagram::unbound()?;
        socket.connect(path)?;
        Ok(Self(socket))
    }

    pub fn from_owned_fd(fd: OwnedFd) -> Self {
        Self(UnixDatagram::from(fd))
    }

    /// Adopts a descriptor this process already owns as the rendezvous it was granted.
    ///
    /// The same classification as [`Socket::granted`], reached without a number anyone announced: a host
    /// that places a rendezvous in a process by its own means — a shell's redirection, a socket the
    /// service manager opened, a supervisor that speaks no evering protocol — hands over the descriptor
    /// and nothing else. Whether that is a meeting is answered by the kernel here exactly as it is in the
    /// announced case, so the two entrances cannot disagree about what a rendezvous is.
    ///
    /// A descriptor that fails the check is closed, because ownership arrived with it. The announced leg
    /// leaves a false claim open for a different reason: there nobody owns the number, and taking it over
    /// would be the claim's doing.
    pub fn from_granted(granted: OwnedFd) -> io::Result<Self> {
        is_rendezvous(&granted.as_fd())?;
        Ok(Self::from_owned_fd(granted))
    }

    /// Adopts the descriptor number a grant announced to this process.
    ///
    /// The number arrives from the environment, so it is a claim rather than authority, and it is
    /// classified through a borrow before anything is owned: only a datagram or seqpacket endpoint is
    /// taken over. A descriptor that is not a socket at all is therefore left exactly as it was, which
    /// is what keeps a false claim from closing something the process opened for its own reasons. A
    /// socket that fails the peer check *is* closed, because the meeting it was offered is over either
    /// way.
    ///
    /// # Safety
    ///
    /// `raw` must name a descriptor that was given to this process by the process that granted it,
    /// and that no other owner will close.
    pub unsafe fn granted(raw: RawFd) -> io::Result<Self> {
        // SAFETY: borrowed, not owned, for exactly as long as it takes to decide whether the claim
        // is a rendezvous at all; ownership is taken only on the path that returns `Ok`.
        let announced = unsafe { BorrowedFd::borrow_raw(raw) };
        is_rendezvous(&announced)?;
        Ok(Self::from_owned_fd(unsafe { OwnedFd::from_raw_fd(raw) }))
    }

    /// What the kernel recorded about the process that owns the peer end.
    ///
    /// On a unix socketpair this is the creator of the pair, captured when the kernel connected
    /// the two ends: neither side can assert it, and neither can revoke it.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub fn peer_credentials(&self) -> io::Result<Credentials> {
        let peer = getsockopt(&self.0, sockopt::PeerCredentials).map_err(io::Error::from)?;
        Ok(Credentials {
            pid: peer.pid() as u32,
            uid: peer.uid(),
            gid: peer.gid(),
        })
    }

    /// Asks the kernel to name, beside every message this end reads, the process that wrote it.
    ///
    /// This is a different answer from `peer_credentials`, and the difference is what a receipt depends
    /// on: the socket's peer field names whoever *created* the pair — the parent, at both ends — while
    /// this names the writer of the one message being read, which on a granted rendezvous can only be the
    /// process the grant was admitted to. The kernel fills the field in from its own records, so the
    /// writer cannot choose it: a `sendmsg` that carries credentials not the sender's own is refused
    /// outright and the message does not arrive.
    ///
    /// The other arms have nothing to ask for, and say so by leaving the endpoint exactly as it is: no
    /// per-message writer is delivered to them by this mechanism, so the receipt that would need it is
    /// refused at `super::handoff` rather than quietly accepted as an uncredited claim.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub fn credit_writers(&self) -> io::Result<()> {
        setsockopt(&self.0, sockopt::PassCred, &true).map_err(io::Error::from)
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    pub fn credit_writers(&self) -> io::Result<()> {
        Ok(())
    }

    pub fn send(&self, bootstrap: &Bootstrap, resources: &[BorrowedFd<'_>]) -> io::Result<()> {
        if resources.len() > MAX_RESOURCES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "too many resources",
            ));
        }
        let mut bytes = Vec::with_capacity(HEADER + bootstrap.as_ref().len());
        bytes.extend_from_slice(&crate::process::HANDOFF_MAGIC);
        bytes.extend_from_slice(&(bootstrap.as_ref().len() as u16).to_le_bytes());
        bytes.extend_from_slice(&(resources.len() as u16).to_le_bytes());
        bytes.extend_from_slice(bootstrap.as_ref());
        let raw: Vec<_> = resources.iter().map(AsRawFd::as_raw_fd).collect();
        self.datagram(&bytes, &raw)
    }

    /// One datagram on this endpoint, with `rights` beside it when there are any.
    ///
    /// The two frames the seam writes — an offer with its descriptors, a receipt with none — differ only
    /// in what rides beside the bytes, so both go through here and neither can drift from the other's
    /// short-write rule.
    fn datagram(&self, bytes: &[u8], rights: &[RawFd]) -> io::Result<()> {
        let data = [IoSlice::new(bytes)];
        let sent = if rights.is_empty() {
            sendmsg::<()>(self.0.as_raw_fd(), &data, &[], MsgFlags::empty(), None)
        } else {
            sendmsg::<()>(
                self.0.as_raw_fd(),
                &data,
                &[ControlMessage::ScmRights(rights)],
                MsgFlags::empty(),
                None,
            )
        }
        .map_err(io::Error::from)?;
        if sent != bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "partial control datagram",
            ));
        }
        Ok(())
    }

    /// Writes the member identity this process arrived as, as one receipt frame with nothing beside it.
    ///
    /// The text is the announcement grammar again, so the receiver parses it with the same function that
    /// parses the environment key and refuses a scribble in it the same way. No credentials are attached:
    /// the kernel fills the writer's identity in when the reading end has asked for it, which is why a
    /// writer cannot name a process other than itself — and why this call has nothing to say about who
    /// is reading.
    pub fn report(&self, announcement: &str) -> io::Result<()> {
        if announcement.len() > MAX_BOOTSTRAP {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "receipt text too long",
            ));
        }
        let mut bytes = Vec::with_capacity(HEADER + announcement.len());
        bytes.extend_from_slice(&crate::process::ARRIVAL_MAGIC);
        bytes.extend_from_slice(&(announcement.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(announcement.as_bytes());
        self.datagram(&bytes, &[])
    }

    /// Reads one receipt frame and the kernel's answer about the process that wrote it, by `deadline`.
    ///
    /// The bound is a `poll` in front of a non-blocking read rather than a socket option, so nothing is
    /// left set on the endpoint behind the caller's back and the pair's other end keeps its own timing.
    /// `poll` takes whole milliseconds and truncates, so a bound past its ceiling is taken in pieces: each
    /// wake re-measures against `deadline`, which is what keeps a long one from ending early.
    ///
    /// Whether a departure is answerable at all depends on what kind of endpoint this is, and the two were
    /// measured rather than assumed. On a seqpacket pair the kernel reports the peer gone as a readable
    /// zero-byte datagram, which is answered here as an end rather than retried. On a datagram pair — the
    /// kind `Socket::pair` makes, so the kind every handover this file carries crosses — it reports nothing:
    /// no readiness, no hangup, only `EAGAIN` to a read. A parent waiting on a child that met and left
    /// without writing is therefore answered by its own `deadline` and by no signal from the host, which is
    /// why the bound is a parameter of this call rather than a policy inside it.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub fn arrival(&self, deadline: Instant) -> io::Result<(Introduction, Credentials)> {
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let budget = PollTimeout::from(remaining.as_millis().min(u16::MAX as u128) as u16);
            let mut watched = [PollFd::new(self.as_fd(), PollFlags::POLLIN)];
            match poll(&mut watched, budget) {
                Ok(0) => continue,
                Ok(_) => {}
                // A signal is not a receipt, and an interrupted park is retried rather than reported.
                Err(Errno::EINTR) => continue,
                Err(error) => return Err(error.into()),
            }
            if watched[0].revents().is_none_or(|flags| flags.is_empty()) {
                continue;
            }
            return self.read_receipt();
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn read_receipt(&self) -> io::Result<(Introduction, Credentials)> {
        let mut bytes = [0; HEADER + MAX_BOOTSTRAP];
        let mut data = [IoSliceMut::new(&mut bytes)];
        let mut control = cmsg_space!(UnixCredentials);
        let message = recvmsg::<()>(
            self.0.as_raw_fd(),
            &mut data,
            Some(&mut control),
            MsgFlags::MSG_DONTWAIT | MsgFlags::MSG_CMSG_CLOEXEC,
        )
        .map_err(io::Error::from)?;
        let count = message.bytes;
        let flags = message.flags;
        let mut credited = None;
        for item in message.cmsgs().map_err(io::Error::from)? {
            match item {
                ControlMessageOwned::ScmCredentials(creds) => {
                    credited = Some(Credentials {
                        pid: creds.pid() as u32,
                        uid: creds.uid(),
                        gid: creds.gid(),
                    });
                }
                ControlMessageOwned::ScmRights(_) => {
                    return Err(invalid("a receipt carries no descriptors"));
                }
                _ => {}
            }
        }
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the peer closed the rendezvous without reporting its arrival",
            ));
        }
        if flags.intersects(MsgFlags::MSG_TRUNC) {
            return Err(invalid("truncated receipt"));
        }
        let credentials = credited.ok_or_else(|| {
            invalid("the receipt arrived with no answer about the process that wrote it")
        })?;
        if count < HEADER || bytes[..4] != crate::process::ARRIVAL_MAGIC {
            return Err(invalid("malformed receipt header"));
        }
        let len = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
        let declared = u16::from_le_bytes([bytes[6], bytes[7]]) as usize;
        if len > MAX_BOOTSTRAP || count != HEADER + len || declared != 0 {
            return Err(invalid("malformed receipt length"));
        }
        let text = std::str::from_utf8(&bytes[HEADER..count])
            .map_err(|_| invalid("a receipt is not text"))?;
        let arrived = Introduction::announced(text)
            .ok_or_else(|| invalid("the receipt names no member identity"))?;
        Ok((arrived, credentials))
    }

    pub fn recv(&self, expected: usize) -> io::Result<Offer> {
        if expected > MAX_RESOURCES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "too many expected resources",
            ));
        }
        let mut bytes = [0; HEADER + MAX_BOOTSTRAP];
        let mut data = [IoSliceMut::new(&mut bytes)];
        let mut control = cmsg_space!([i32; MAX_RESOURCES]);
        let message = recvmsg::<()>(
            self.0.as_raw_fd(),
            &mut data,
            Some(&mut control),
            MsgFlags::MSG_CMSG_CLOEXEC,
        )
        .map_err(io::Error::from)?;
        let count = message.bytes;
        let flags = message.flags;
        let mut resources = Vec::new();
        for control in message.cmsgs().map_err(io::Error::from)? {
            if let ControlMessageOwned::ScmRights(received) = control {
                resources.extend(
                    received
                        .into_iter()
                        .map(|fd| unsafe { OwnedFd::from_raw_fd(fd) }),
                );
            }
        }
        if flags.intersects(MsgFlags::MSG_TRUNC | MsgFlags::MSG_CTRUNC) {
            return Err(invalid("truncated control datagram"));
        }
        if count < HEADER || bytes[..4] != crate::process::HANDOFF_MAGIC {
            return Err(invalid("malformed control header"));
        }
        let len = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
        let declared = u16::from_le_bytes([bytes[6], bytes[7]]) as usize;
        if len > MAX_BOOTSTRAP || count != HEADER + len {
            return Err(invalid("malformed bootstrap length"));
        }
        if declared > MAX_RESOURCES || declared != expected || resources.len() != expected {
            return Err(invalid("resource count mismatch"));
        }
        for fd in &resources {
            fcntl(fd, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC)).map_err(io::Error::from)?;
        }
        let bootstrap =
            Bootstrap::new(&bytes[HEADER..count]).map_err(|_| invalid("bootstrap too large"))?;
        Ok(Offer {
            bootstrap,
            resources: resources.into_boxed_slice(),
        })
    }
}

impl AsFd for Socket {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl Offer {
    pub fn bootstrap(&self) -> &Bootstrap {
        &self.bootstrap
    }

    pub fn resources(&self) -> &[OwnedFd] {
        &self.resources
    }

    pub fn into_parts(self) -> (Bootstrap, Box<[OwnedFd]>) {
        (self.bootstrap, self.resources)
    }
}
