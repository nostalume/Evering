#![cfg(all(feature = "process", feature = "notify", windows))]

use std::{
    env,
    os::windows::io::{AsHandle, AsRawHandle},
    process::Command,
    thread,
    time::{Duration, Instant},
};

use evering::{
    BlockRange, DeclineCause, PoolId, RoomWait, Session,
    layout::RegionId,
    mapping::{Access, Peer, Request},
    notify::Notify,
    os::{
        Handoff, event,
        windows::{
            Section,
            process::{Listener, Socket},
        },
    },
    process::{Bootstrap, Supervisor},
};

mod support;
use crate::support::role;

#[test]
fn a_handoff_names_nothing_and_an_ungranted_process_is_refused() {
    let (handoff, grant) = Handoff::bind().unwrap();
    let mut command = Command::new(env::current_exe().unwrap());
    grant.admit(&mut command).unwrap();
    drop(handoff);
    let error = Handoff::receive()
        .err()
        .expect("a process holding no grant meets no one");
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
}

#[cfg(feature = "notify")]
#[test]
fn received_event_constructor_is_safe() {
    let _: fn(std::os::windows::io::OwnedHandle) -> evering::os::Event =
        evering::os::Event::from_owned_handle;
}
use windows_sys::Win32::{
    Foundation::WAIT_OBJECT_0,
    System::Threading::{
        GetCurrentProcess, GetProcessHandleCount, INFINITE, SetEvent, WaitForSingleObject,
    },
};

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

/// The child's whole program: the roles this claim defines, and nothing else.
#[test]
fn role_entry() {
    role::dispatch(&[
        ("sleeping", sleeping),
        ("taker", taker),
        ("connecting", connecting),
        ("unrelated", unrelated),
        ("counter", counter),
        ("filler", filler),
    ]);
}

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

/// The role the reap claim spawns: fill the pool with blocks this process never releases, then leave
/// without unwinding any of them.
fn filler() -> i32 {
    let socket = Socket::connect(&rendezvous()).unwrap();
    let offer = socket.recv(1).unwrap();
    let id = std::str::from_utf8(offer.bootstrap().as_ref()).unwrap();
    let mut fields = id.split(',');
    let id = PoolId::new(
        REGION,
        fields.next().unwrap().parse().unwrap(),
        fields.next().unwrap().parse().unwrap(),
        fields.next().unwrap().parse().unwrap(),
    );
    let (_, resources) = offer.into_parts();
    let mut resources = resources.into_vec();
    let section = Section::from_owned_handle(resources.remove(0));
    let session = Session::open(
        section,
        Request::new(SIZE, Access::READ | Access::WRITE),
        REGION,
    )
    .unwrap();
    let peer = session.peer();
    let pool = session.open_pool(id).unwrap();
    let pool_ref = pool.as_ref();
    let mut blocks = Vec::new();
    let shortage = DeclineCause::NoRoom {
        waits: RoomWait::Release,
    };
    loop {
        match pool_ref.put(7_u64) {
            Ok(block) => blocks.push(block),
            Err(decline) if decline.cause == shortage => break,
            Err(decline) => panic!("unexpected Pool decline: {:?}", decline.cause),
        }
    }
    std::fs::write(
        env::var(READY).unwrap(),
        format!("{},{},{}", peer.slot(), peer.generation(), blocks.len()),
    )
    .unwrap();
    core::mem::forget(blocks);
    core::mem::forget(pool);
    core::mem::forget(session);
    0
}

#[test]
fn exited_process_pool_ownership_is_reclaimed() {
    let ready = env::temp_dir().join(format!("evering-pool-ready-{}", std::process::id()));
    let _ = std::fs::remove_file(&ready);
    let listener = Listener::bind().unwrap();
    let mut command = role::child("filler");
    command.env(RENDEZVOUS, listener.name()).env(READY, &ready);
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
    let pool = session
        .create_pool(64 * 1024, Some(BlockRange::new(64, 64).unwrap()))
        .unwrap();
    let id = pool.id();
    let (_, slab, entry, generation) = id.parts();
    let id = format!("{slab},{entry},{generation}");
    socket
        .send(
            &child,
            &Bootstrap::new(id.as_bytes()).unwrap(),
            &[section.as_handle()],
        )
        .unwrap();
    let exit = child.wait().unwrap();
    assert!(exit.success());
    let identity = std::fs::read_to_string(&ready).unwrap();
    std::fs::remove_file(&ready).unwrap();
    let mut fields = identity.split(',');
    let peer = Peer::from_parts(
        fields.next().unwrap().parse().unwrap(),
        fields.next().unwrap().parse().unwrap(),
    );
    assert!(fields.next().unwrap().parse::<usize>().unwrap() >= 64);
    assert_eq!(pool.id(), PoolId::new(REGION, slab, entry, generation));
    let decline = pool
        .as_ref()
        .put(9_u64)
        .err()
        .expect("a dead member's claims still occupy the pool");
    assert_eq!(
        decline.cause,
        DeclineCause::NoRoom {
            waits: RoomWait::Release
        },
        "the pool is full of one member's claims, so what frees room is a release"
    );
    assert_eq!(decline.value, Some(9_u64), "the declined block comes back");
    let recovery = unsafe { session.assume_dead(peer) }.unwrap();
    assert!(recovery.reap().is_ok());
    assert_eq!(*pool.as_ref().put(11_u64).unwrap(), 11);
}
