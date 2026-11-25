//! What the control pipe carries besides bytes: an offer's opaque prefix and its descriptors arrive together
//! or not at all, and a refusal closes whatever it already received. Two of the refusals here are arranged
//! by hand rather than asked for politely — one receives with a count the datagram does not carry, the other
//! writes its frame straight with `sendmsg` so that rights arrive beside bytes no parser in this crate
//! claims — and both end with the peer's own end of a stream reporting EOF, which is the only way to see a
//! descriptor the kernel dup'd *into* this process again leave it.
//!
//! One seam differs from the Windows leg, and it is the reason the two stay separate files rather than one
//! file with two arms: here the child binds the rendezvous and the parent connects back to a name in the
//! filesystem, while there the parent owns a named pipe instance and the child dials it. The bounds case
//! stays inside this process on both hosts, because a local limit that rejected nothing would otherwise
//! look like a transport failure.
//!
//! Two questions this pipe does not answer, named so their absence is not read as coverage: which member a
//! handover named, which `handover_admission` answers, and what a death left behind, which
//! `recovery_conserves` counts.

use std::{
    env,
    fs::File,
    io::{IoSlice, Read},
    os::{
        fd::{AsFd, AsRawFd},
        unix::net::UnixStream,
    },
    thread,
    time::{Duration, Instant},
};

use evering::{
    os::unix::process::Socket,
    process::{Bootstrap, MAX_BOOTSTRAP, MAX_RESOURCES},
};

use nix::{
    fcntl::{FcntlArg, FdFlag, fcntl},
    sys::socket::{ControlMessage, MsgFlags, sendmsg},
};

use crate::support::role;

/// The rendezvous a role is told to bind, which is the same name the parent connects back to.
const RENDEZVOUS: &str = "EVERING_EXCHANGE_RENDEZVOUS";
/// The size of the mapping the `taker` role is handed, and the size it checks that file against.
const SIZE: usize = 4096;

#[test]
fn received_event_constructor_is_safe() {
    let _: fn(std::os::fd::OwnedFd) -> evering::os::Event = evering::os::Event::from_owned_fd;
}

fn assert_peer_closed(observer: &mut UnixStream) {
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut byte = [0];
    loop {
        match observer.read(&mut byte) {
            Ok(0) => return,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::yield_now();
            }
            result => panic!("received descriptor remained open: {result:?}"),
        }
    }
}

#[test]
fn transfers_exact_owned_resources_and_opaque_bytes() {
    let (send, recv) = Socket::pair().unwrap();
    let first = File::open("/dev/null").unwrap();
    let second = File::open("/dev/zero").unwrap();
    let bootstrap = Bootstrap::new(b"caller-defined").unwrap();

    send.send(&bootstrap, &[first.as_fd(), second.as_fd()])
        .unwrap();
    let offer = recv.recv(2).unwrap();

    assert_eq!(offer.bootstrap().as_ref(), b"caller-defined");
    assert_eq!(offer.resources().len(), 2);
    assert!(offer.resources().iter().all(|fd| fd.as_raw_fd() >= 0));
    assert!(offer.resources().iter().all(|fd| {
        FdFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFD).unwrap())
            .contains(FdFlag::FD_CLOEXEC)
    }));
}

#[test]
fn exact_count_failure_closes_every_received_resource() {
    let (send, recv) = Socket::pair().unwrap();
    let (mut observer, offered) = UnixStream::pair().unwrap();
    observer.set_nonblocking(true).unwrap();
    let bootstrap = Bootstrap::new([]).unwrap();

    send.send(&bootstrap, &[offered.as_fd()]).unwrap();
    assert_eq!(
        recv.recv(2).unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );
    drop(offered);

    assert_peer_closed(&mut observer);
}

#[test]
fn malformed_offer_closes_attached_resources() {
    let (send, recv) = Socket::pair().unwrap();
    let (mut observer, offered) = UnixStream::pair().unwrap();
    observer.set_nonblocking(true).unwrap();
    let bytes = [IoSlice::new(b"not-an-evering-offer")];
    let raw = [offered.as_raw_fd()];

    sendmsg::<()>(
        send.as_fd().as_raw_fd(),
        &bytes,
        &[ControlMessage::ScmRights(&raw)],
        MsgFlags::empty(),
        None,
    )
    .unwrap();
    assert_eq!(
        recv.recv(1).unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );
    drop(offered);

    assert_peer_closed(&mut observer);
}

#[test]
fn local_bounds_refuse_without_transport() {
    assert!(Bootstrap::new(vec![0; MAX_BOOTSTRAP + 1]).is_err());
    let (send, _) = Socket::pair().unwrap();
    let file = File::open("/dev/null").unwrap();
    let resources = vec![file.as_fd(); MAX_RESOURCES + 1];
    assert_eq!(
        send.send(&Bootstrap::new([]).unwrap(), &resources)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
}

/// The role the handover spawns: take the region and the two bells the parent sends, and ring back.
fn taker() -> i32 {
    use evering::os::unix::UnixFd;
    use nix::{sys::stat::fstat, unistd};

    let socket = Socket::bind(env::var(RENDEZVOUS).unwrap()).unwrap();
    let offer = socket.recv(3).unwrap();
    assert_eq!(offer.bootstrap().as_ref(), b"selected-protocol-id");
    let (_, resources) = offer.into_parts();
    let mut resources = resources.into_vec();
    let mapping = UnixFd::from_fd(resources.remove(0)).unwrap();
    assert_eq!(mapping.size(), SIZE);
    assert_eq!(fstat(mapping.as_fd()).unwrap().st_size as usize, SIZE);

    let mut word = [0; 8];
    let arrived = unistd::read(&resources[0], &mut word).unwrap();
    assert!(
        arrived > 0,
        "the wait end the child received must still hold the kick queued before the handoff"
    );
    assert_eq!(
        unistd::write(&resources[1], &1u64.to_ne_bytes()).unwrap(),
        8
    );
    0
}

/// The roles this leg defines, kept beside their bodies. The claim root holds the target's only
/// `role_entry`, because the harness re-enters a child by exact item name and only one item in a target may
/// answer to it.
pub(crate) const ROLES: &[(&str, role::Body)] = &[("taker", taker)];

#[test]
fn child_accepts_mapping_and_directional_bells() {
    use evering::{
        notify::Notify,
        os::{event, unix::UnixFd},
        process::Supervisor,
    };
    use nix::unistd;

    let path = env::temp_dir().join(format!("evering-exchange-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let mut command = role::child("taker");
    command.env(RENDEZVOUS, &path);
    let mut child = Supervisor::spawn(&mut command).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let socket = loop {
        match Socket::connect(&path) {
            Ok(socket) => break socket,
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("child rendezvous failed: {error}"),
        }
    };

    let mapping = UnixFd::memfd("exchange", SIZE, false).unwrap();
    let (to_child, child_wait) = event().unwrap();
    let (child_ring, from_child) = event().unwrap();
    to_child.notify().unwrap();
    socket
        .send(
            &Bootstrap::new(b"selected-protocol-id").unwrap(),
            &[mapping.as_fd(), child_wait.as_fd(), child_ring.as_fd()],
        )
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut word = [0; 8];
    loop {
        match unistd::read(from_child.as_fd(), &mut word) {
            Ok(8) => break,
            Err(nix::errno::Errno::EAGAIN) if Instant::now() < deadline => thread::yield_now(),
            result => panic!("child notification failed: {result:?}"),
        }
    }
    assert_eq!(u64::from_ne_bytes(word), 1);
    assert!(child.wait().unwrap().success());
    std::fs::remove_file(path).unwrap();
}
