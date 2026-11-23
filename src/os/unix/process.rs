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

#[cfg(target_os = "linux")]
use crate::process::Writer;

const HEADER: usize = 8;

/// The `SOL_SOCKET` cmsg type that carries the ancillary `pidfd` `SO_PASSPIDFD` delivers beside a credited
/// receipt. `nix` 0.30.1 decodes no cmsg of this type, so it surfaces as `ControlMessageOwned::Unknown` and
/// its descriptor has to be taken out here — `UnknownCmsg` owns no `Drop`.
#[cfg(target_os = "linux")]
const SCM_PIDFD: nix::libc::c_int = 4;

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

    /// Asks the kernel to hand over, beside each credited receipt, a `pidfd` naming the process that wrote
    /// it — `credit_writers` turned from an answer into an object.
    ///
    /// `SO_PASSCRED` makes the kernel *say* which process wrote; `SO_PASSPIDFD` makes it pass that process's
    /// `pidfd` in the same message, so a reader that is not the writer's parent still ends up holding a live
    /// reference the kernel chose at delivery rather than a number it looked up afterward. That reference is
    /// what `super::handoff`'s `introduction` reads out and what `crate::process::Writer` wraps.
    ///
    /// Linux only, for the same reason `crate::process`'s `watch` opens its `pidfd` by syscall: the socket
    /// option has no wrapper in the `nix` features this crate enables, and `SO_PASSPIDFD_THREAD` measured as
    /// `ENOPROTOOPT` on the reference host, so the granularity is a thread group and no thread-only arm is
    /// written. Widening `read_receipt`'s control buffer is mandatory alongside this call — a buffer sized
    /// for credentials alone truncates the ancillary list, and the kernel answers a truncated control message
    /// by discarding every cmsg in it — so arming the option without the widening fails every receipt.
    #[cfg(target_os = "linux")]
    pub fn watch_writers(&self) -> io::Result<()> {
        const SO_PASSPIDFD: nix::libc::c_int = 76;
        let enabled: nix::libc::c_int = 1;
        // SAFETY: `self.0` is this endpoint's own descriptor, `SO_PASSPIDFD` is a `SOL_SOCKET` option that
        // takes a single `c_int`, and `enabled` is a live local whose address is read only for the call.
        let result = unsafe {
            nix::libc::setsockopt(
                self.0.as_raw_fd(),
                nix::libc::SOL_SOCKET,
                SO_PASSPIDFD,
                &enabled as *const nix::libc::c_int as *const nix::libc::c_void,
                std::mem::size_of::<nix::libc::c_int>() as nix::libc::socklen_t,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
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

    /// Reads one receipt frame, the kernel's answer about the process that wrote it, and — on Linux, once
    /// this endpoint is armed for it — a live reference to that process, by `deadline`.
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
    ///
    /// The third slot changes nothing about the wire: it is `None` until `watch_writers` is called on this
    /// endpoint, and the bytes the peer sends are the same either way. It is `Some` exactly when the kernel
    /// delivered a `pidfd` beside the credentials, which is when an introduced — not spawned — writer's
    /// death becomes something the reading process can prove without reaping it.
    #[cfg(target_os = "linux")]
    pub fn arrival(
        &self,
        deadline: Instant,
    ) -> io::Result<(Introduction, Credentials, Option<Writer>)> {
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

    #[cfg(target_os = "linux")]
    fn read_receipt(&self) -> io::Result<(Introduction, Credentials, Option<Writer>)> {
        let mut bytes = [0; HEADER + MAX_BOOTSTRAP];
        let mut data = [IoSliceMut::new(&mut bytes)];
        // Room for the writer's credentials *and* the `pidfd` `SO_PASSPIDFD` puts beside them. A buffer
        // sized for credentials alone truncates the ancillary list, and the kernel answers a truncated
        // control message by discarding every cmsg it held — so arming the option without widening here is
        // the one ordering this file cannot get wrong, and why the two change together.
        let mut control = cmsg_space!(UnixCredentials, nix::libc::c_int);
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
        let mut writer = None;
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
                // `SO_PASSPIDFD`'s data is not a variant `nix` 0.30.1 decodes, so it surfaces as `Unknown`
                // with the `pidfd` packed into its data bytes — and `UnknownCmsg` owns no `Drop`, so the
                // descriptor has to be taken out here or every receipt leaks one into this process's table.
                ControlMessageOwned::Unknown(unknown)
                    if unknown.cmsg_header.cmsg_type == SCM_PIDFD
                        && unknown.data_bytes.len() >= std::mem::size_of::<RawFd>() =>
                {
                    let mut raw = [0u8; std::mem::size_of::<RawFd>()];
                    raw.copy_from_slice(&unknown.data_bytes[..std::mem::size_of::<RawFd>()]);
                    let fd = RawFd::from_ne_bytes(raw);
                    // SAFETY: the kernel delivered `fd` as this receipt's writer `pidfd`, `MSG_CMSG_CLOEXEC`
                    // marked it close-on-exec, and nothing else in this process holds it, so `OwnedFd` may
                    // undertake to close it.
                    writer = Some(Writer::received(unsafe { OwnedFd::from_raw_fd(fd) }));
                }
                // An ancillary item this endpoint was not told to expect — including an `Unknown` that is not
                // a pidfd. There is nothing to hand back for it and no descriptor it could have smuggled, so
                // it is dropped with the message.
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
        Ok((arrived, credentials, writer))
    }

    /// The same receipt read on Android, which answers `SO_PASSCRED` but delivers no `pidfd`: the writer's
    /// identity and nothing more, so the control buffer stays sized for credentials alone and the reference
    /// half of `Death`'s second entrance is not reachable here.
    #[cfg(target_os = "android")]
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

    #[cfg(target_os = "android")]
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

/// How many descriptors this process currently holds that name a `pidfd`.
///
/// Counted by the symlink every `/proc/self/fd` entry points at, so only a kernel-delivered writer reference
/// is visible here — no other test in this crate opens a `pidfd`, which is what makes the count a sound signal
/// in a suite that runs its cases on parallel threads. A process-wide descriptor total would not be: another
/// case's memfd, open mid-flight, would move it.
#[cfg(all(test, target_os = "linux"))]
fn pidfd_descriptors() -> usize {
    let Ok(entries) = std::fs::read_dir("/proc/self/fd") else {
        return usize::MAX;
    };
    entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            std::fs::read_link(entry.path())
                .map(|target| target.to_string_lossy().contains("pidfd"))
                .unwrap_or(false)
        })
        .count()
}

/// The one ordering `Socket` cannot get wrong, measured rather than documented: `SO_PASSPIDFD` and the widened
/// control buffer are a single change, and a receipt read has to take the `pidfd` the kernel hands it.
///
/// Two claims, read off the same armed endpoint. A frame too short to pass the grammar is refused *for the
/// frame* — which only happens if the credentials survived the `recvmsg`, and a control buffer sized for
/// credentials alone would have truncated the ancillary list once `SO_PASSPIDFD` added a second cmsg, discarding
/// every one of them and producing a different error. And across 100 such reads no `pidfd` descriptor is left
/// open: the reference the kernel delivers is taken into a `Writer` and closed when the read returns, which is
/// what `ControlMessageOwned::Unknown` owning no `Drop` makes this file responsible for rather than the kernel.
#[cfg(all(test, target_os = "linux"))]
#[test]
fn an_armed_endpoint_reads_every_receipt_and_keeps_no_writers_descriptor() {
    let (reader, writer) = Socket::pair().expect("a connected datagram pair");
    reader.credit_writers().expect("name each writer");
    reader.watch_writers().expect("deliver each writer's pidfd");

    // A well-formed receipt read first, so the leak check below is not vacuous: it proves this endpoint really
    // does receive a writer reference, which is the only thing there is to leak.
    writer
        .report("7,3,1,2")
        .expect("send one well-formed receipt");
    let (_, _, delivered) = reader
        .read_receipt()
        .expect("read the armed receipt the writer just put down");
    assert!(
        delivered.is_some(),
        "the armed endpoint delivered no writer pidfd, so a flat descriptor count below would prove nothing"
    );
    drop(delivered);

    let not_a_receipt = [0u8; 4];
    let before = pidfd_descriptors();
    for _ in 0..100 {
        writer.datagram(&not_a_receipt, &[]).expect("send a frame");
        let error = match reader.read_receipt() {
            Ok(_) => panic!("a four-byte frame was read as a receipt"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("malformed receipt header"),
            "the armed read refused the receipt for the wrong reason ({error}); a control buffer that had \
             truncated the ancillary list would report a missing writer, not the frame — the widening has to \
             travel with the arming"
        );
    }
    assert_eq!(
        pidfd_descriptors(),
        before,
        "100 armed receipts grew this process's pidfd descriptors: each reference the kernel delivered has to \
         be taken into a `Writer`, which closes it when the read returns, not dropped beside the message"
    );
}
