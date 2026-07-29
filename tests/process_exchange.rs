#![cfg(all(feature = "process", unix))]

use std::{
    env,
    fs::File,
    io::{IoSlice, Read},
    os::{
        fd::{AsFd, AsRawFd},
        unix::net::UnixStream,
    },
    process::Command,
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

    let mut byte = [0];
    assert_eq!(observer.read(&mut byte).unwrap(), 0);
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

    let mut byte = [0];
    assert_eq!(observer.read(&mut byte).unwrap(), 0);
}

#[test]
fn local_bounds_reject_without_transport() {
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

#[cfg(feature = "notify")]
#[test]
fn child_accepts_mapping_and_directional_bells() {
    use evering::{
        Notify,
        os::{event, unix::UnixFd},
        process::Supervisor,
    };
    use nix::{sys::stat::fstat, unistd};

    const CHILD: &str = "EVERING_EXCHANGE_CHILD";
    const SIZE: usize = 4096;

    if let Some(path) = env::var_os(CHILD) {
        let socket = Socket::bind(path).unwrap();
        let offer = socket.recv(3).unwrap();
        assert_eq!(offer.bootstrap().as_ref(), b"selected-protocol-id");
        let (_, resources) = offer.into_parts();
        let mut resources = resources.into_vec();
        let mapping = UnixFd::from_fd(resources.remove(0)).unwrap();
        assert_eq!(mapping.size(), SIZE);
        assert_eq!(fstat(mapping.as_fd()).unwrap().st_size as usize, SIZE);

        let mut word = [0; 8];
        assert_eq!(unistd::read(&resources[0], &mut word).unwrap(), 8);
        assert_eq!(u64::from_ne_bytes(word), 1);
        assert_eq!(
            unistd::write(&resources[1], &1u64.to_ne_bytes()).unwrap(),
            8
        );
        return;
    }

    let path = env::temp_dir().join(format!("evering-exchange-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let mut command = Command::new(env::current_exe().unwrap());
    command
        .args(["--exact", "child_accepts_mapping_and_directional_bells"])
        .env(CHILD, &path);
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
