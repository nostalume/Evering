use std::{env, io, os::windows::io::AsHandle, process::Command, time::Instant};

use crate::{
    os::{
        Event, Peer, ReceivedResources, Ring,
        windows::{
            Section,
            process::{Listener, Socket},
        },
    },
    process::{Bootstrap, INTRODUCER, RENDEZVOUS, Supervisor},
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
pub struct Grant {
    address: String,
    introducer: u32,
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
            },
        ))
    }

    /// Moves the region, the doorbell and the ring to the process the grant was admitted to.
    ///
    /// `accept` asks the kernel whose client connected and refuses anything that is not `child`, and
    /// `deadline` bounds the wait for that client to appear — the pipe instance exists long before
    /// the child has run.
    pub fn send<F: AsHandle>(
        self,
        child: &Supervisor,
        deadline: Instant,
        bootstrap: &Bootstrap,
        mapping: &Section<F>,
        event: &Event,
        ring: &Ring,
    ) -> io::Result<()> {
        self.listener.accept(child, deadline)?.send(
            child,
            bootstrap,
            &[mapping.as_handle(), event.as_handle(), ring.as_handle()],
        )
    }

    /// Takes the handover this process was granted, and attests the peer before any of it is
    /// released to the caller.
    pub fn receive() -> io::Result<(Peer, ReceivedResources)> {
        let address = env::var(RENDEZVOUS)
            .map_err(|_| refused("no evering rendezvous was granted to this process"))?;
        let introducer: u32 = env::var(INTRODUCER)
            .map_err(|_| refused("the granted rendezvous names no introducer"))?
            .parse()
            .map_err(|_| refused("the announced introducer is not a process id"))?;
        let socket = Socket::connect(&address)?;
        let server = socket
            .server_pid()
            .map_err(|_| refused("the granted rendezvous reports no server"))?;
        if server != introducer {
            return Err(refused(
                "the peer behind the granted rendezvous is not the process that granted it",
            ));
        }
        let (bootstrap, resources) = socket.recv(3)?.into_parts();
        let mut resources = resources.into_vec();
        Ok((
            Peer::introduced(server),
            ReceivedResources {
                bootstrap,
                mapping: Section::from_owned_handle(resources.remove(0)),
                event: Event::from_owned_handle(resources.remove(0)),
                ring: Ring::from_owned_handle(resources.remove(0)),
            },
        ))
    }
}

impl Grant {
    /// Installs this rendezvous in `command`, so the process it spawns is the one meeting.
    ///
    /// Consuming `self` is what makes the grant single-use: a second `admit` cannot be written, and
    /// the address never becomes something a caller threads through arguments, where every process
    /// on the host could read it.
    pub fn admit(self, command: &mut Command) -> io::Result<()> {
        command.env(RENDEZVOUS, self.address.as_str());
        command.env(INTRODUCER, self.introducer.to_string());
        Ok(())
    }
}
