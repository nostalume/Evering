//! The Windows control pipe, asked from both ends. A `Listener` owns one pipe instance and hands its name to
//! one command, then refuses a client that is not the child it was told about — `PermissionDenied` against a
//! retained child that is somebody else's — and owes a timeout to a child that never dials at all. Neither
//! face has a unix counterpart in this claim: a `Socket::pair` is already the connection, so there is no
//! instance to compare a client against and no `accept` left to run out.
//!
//! What arrives with an offer is counted rather than trusted. The child reads its own handle table before
//! and after a receive that asked for more handles than the frame carries, so the refusal is shown to close
//! what it received instead of parking a duplicate. The handover case keeps the unix leg's shape — a section
//! handle plus one event each way — with the dial reversed: here the parent binds and the child connects.

use std::{
    env,
    os::windows::io::{AsHandle, AsRawHandle},
    thread,
    time::{Duration, Instant},
};

use evering::{
    Session,
    layout::RegionId,
    mapping::{Access, Request},
    notify::Notify,
    os::{
        event,
        windows::{
            Section,
            process::{Listener, Socket},
        },
    },
    process::{Bootstrap, Supervisor},
};

use windows_sys::Win32::{
    Foundation::WAIT_OBJECT_0,
    System::Threading::{
        GetCurrentProcess, GetProcessHandleCount, INFINITE, SetEvent, WaitForSingleObject,
    },
};

use crate::support::role;

#[test]
fn received_event_constructor_is_safe() {
    let _: fn(std::os::windows::io::OwnedHandle) -> evering::os::Event =
        evering::os::Event::from_owned_handle;
}

const SIZE: usize = 4 * 1024 * 1024;
const REGION: RegionId = RegionId::new(0x5749_4e44_4f57_534d, 2);
/// The control pipe a role is pointed at, which each role that meets connects to.
const RENDEZVOUS: &str = "EVERING_WINDOWS_EXCHANGE_NAME";
/// The file a role writes once it is ready, so a parent that has to act between two of the child's
/// steps can tell when to act.
const READY: &str = "EVERING_WINDOWS_EXCHANGE_READY";

/// The pipe a role is to connect to, named the only way a second process can hear it.
fn rendezvous() -> String {
    env::var(RENDEZVOUS).expect("a role that meets is told which pipe to connect to")
}

/// The roles this leg defines, kept beside their bodies. The claim root holds the target's only
/// `role_entry`, because the harness re-enters a child by exact item name and only one item in a target may
/// answer to it.
pub(crate) const ROLES: &[(&str, role::Body)] = &[
    ("sleeping", sleeping),
    ("taker", taker),
    ("connecting", connecting),
    ("unrelated", unrelated),
    ("counter", counter),
];

/// The role the deadline claim spawns: stay alive without ever connecting, so an `accept` that is owed a
/// timeout is given one.
fn sleeping() -> i32 {
    thread::sleep(Duration::from_secs(1));
    0
}

#[test]
fn control_pipe_accept_obeys_its_deadline() {
    let listener = Listener::bind().unwrap();
    let child = Supervisor::spawn(&mut role::child("sleeping")).unwrap();
    assert_eq!(
        listener.accept(&child, Instant::now()).unwrap_err().kind(),
        std::io::ErrorKind::TimedOut
    );
}

fn handle_count() -> u32 {
    let mut count = 0;
    assert_ne!(
        unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) },
        0
    );
    count
}

/// The role the section handover spawns: open the region the parent sends, wait on one bell and ring the
/// other.
fn taker() -> i32 {
    let socket = Socket::connect(&rendezvous()).unwrap();
    let offer = socket.recv(3).unwrap();
    assert_eq!(offer.bootstrap().as_ref(), b"selected-protocol-id");
    let (_, resources) = offer.into_parts();
    let mut resources = resources.into_vec();
    let section = Section::from_owned_handle(resources.remove(0));
    let session = Session::open(
        section,
        Request::new(SIZE, Access::READ | Access::WRITE),
        REGION,
    )
    .unwrap();
    assert_ne!(session.base_addr(), 0);
    assert_eq!(
        unsafe { WaitForSingleObject(resources[0].as_raw_handle().cast(), INFINITE) },
        WAIT_OBJECT_0
    );
    assert_ne!(unsafe { SetEvent(resources[1].as_raw_handle().cast()) }, 0);
    0
}

#[test]
fn child_accepts_section_and_directional_events() {
    let listener = Listener::bind().unwrap();
    let mut command = role::child("taker");
    command.env(RENDEZVOUS, listener.name());
    let mut child = Supervisor::spawn(&mut command).unwrap();
    let socket = listener
        .accept(&child, Instant::now() + Duration::from_secs(5))
        .unwrap();

    let section = Section::anonymous(SIZE, Access::READ | Access::WRITE).unwrap();
    let session = Session::create(
        section.borrow(),
        Request::new(SIZE, Access::READ | Access::WRITE),
        REGION,
    )
    .unwrap();

    let (to_child, child_wait) = event().unwrap();
    let (child_ring, from_child) = event().unwrap();
    socket
        .send(
            &child,
            &Bootstrap::new(b"selected-protocol-id").unwrap(),
            &[
                section.as_handle(),
                child_wait.as_handle(),
                child_ring.as_handle(),
            ],
        )
        .unwrap();
    drop(section);
    to_child.notify().unwrap();
    assert_eq!(
        unsafe { WaitForSingleObject(from_child.as_raw_handle().cast(), INFINITE) },
        WAIT_OBJECT_0
    );
    assert!(child.wait().unwrap().success());
    assert_ne!(session.base_addr(), 0);
}

/// The role the identity claim spawns as the participant the pipe was opened for: connect and hold, so
/// the parent has a real instance to name.
fn connecting() -> i32 {
    let _socket = Socket::connect(&rendezvous()).unwrap();
    thread::sleep(Duration::from_secs(1));
    0
}

/// The role the identity claim spawns as somebody else: it never connects, so the only client on the
/// pipe belongs to a process the retained child is not.
fn unrelated() -> i32 {
    loop {
        thread::sleep(Duration::from_secs(1));
    }
}

#[test]
fn control_pipe_rejects_a_different_child_instance() {
    let listener = Listener::bind().unwrap();
    let mut connecting = role::child("connecting");
    connecting.env(RENDEZVOUS, listener.name());
    let connecting = Supervisor::spawn(&mut connecting).unwrap();

    let unrelated = Supervisor::spawn(&mut role::child("unrelated")).unwrap();

    assert_eq!(
        listener
            .accept(&unrelated, Instant::now() + Duration::from_secs(5))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::PermissionDenied
    );
    drop(connecting);
    drop(unrelated);
}

/// The role the count claim spawns: ask for two resources when one is coming, and show the handle that
/// arrived did not outlive the refusal.
fn counter() -> i32 {
    let socket = Socket::connect(&rendezvous()).unwrap();
    let before = handle_count();
    std::fs::File::create(env::var(READY).unwrap()).unwrap();
    assert_eq!(
        socket.recv(2).unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );
    assert_eq!(handle_count(), before);
    0
}

#[test]
fn count_mismatch_closes_every_remote_handle() {
    let ready = env::temp_dir().join(format!("evering-exchange-ready-{}", std::process::id()));
    let _ = std::fs::remove_file(&ready);
    let listener = Listener::bind().unwrap();
    let mut command = role::child("counter");
    command.env(RENDEZVOUS, listener.name()).env(READY, &ready);
    let mut child = Supervisor::spawn(&mut command).unwrap();
    let socket = listener
        .accept(&child, Instant::now() + Duration::from_secs(5))
        .unwrap();
    while !ready.exists() {
        thread::yield_now();
    }
    let (_, event) = event().unwrap();
    socket
        .send(&child, &Bootstrap::new([]).unwrap(), &[event.as_handle()])
        .unwrap();
    assert!(child.wait().unwrap().success());
    std::fs::remove_file(ready).unwrap();
}
