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
pub use nix::sys::socket::UnixCredentials;

use crate::process::{Bootstrap, MAX_BOOTSTRAP, MAX_RESOURCES};

const HEADER: usize = 8;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
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
        let kind = getsockopt(&announced, sockopt::SockType).map_err(io::Error::from)?;
        if !matches!(kind, SockType::Datagram | SockType::SeqPacket) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the announced descriptor is not a datagram endpoint",
            ));
        }
        Ok(Self::from_owned_fd(unsafe { OwnedFd::from_raw_fd(raw) }))
    }

    /// What the kernel recorded about the process that owns the peer end.
    ///
    /// On a unix socketpair this is the creator of the pair, captured when the kernel connected
    /// the two ends: neither side can assert it, and neither can revoke it.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    pub fn peer_credentials(&self) -> io::Result<UnixCredentials> {
        getsockopt(&self.0, sockopt::PeerCredentials).map_err(io::Error::from)
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
        let data = [IoSlice::new(&bytes)];
        let raw: Vec<_> = resources.iter().map(AsRawFd::as_raw_fd).collect();
        let sent = if raw.is_empty() {
            sendmsg::<()>(self.0.as_raw_fd(), &data, &[], MsgFlags::empty(), None)
        } else {
            sendmsg::<()>(
                self.0.as_raw_fd(),
                &data,
                &[ControlMessage::ScmRights(&raw)],
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
