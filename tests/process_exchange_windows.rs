#![cfg(all(feature = "process", feature = "notify", windows))]

use std::{
    env,
    os::windows::io::{AsHandle, AsRawHandle},
    process::Command,
    thread,
    time::Duration,
};

use evering::{
    Notify, RegionId, Request,
    os::{
        event,
        windows::{
            Section,
            process::{Listener, Socket},
        },
    },
    perlude::talc::{Access, SessionBy},
    process::{Bootstrap, Supervisor},
};
use windows_sys::Win32::{
    Foundation::WAIT_OBJECT_0,
    System::Threading::{
        GetCurrentProcess, GetProcessHandleCount, INFINITE, SetEvent, WaitForSingleObject,
    },
};

const CHILD: &str = "EVERING_WINDOWS_EXCHANGE_CHILD";
const SIZE: usize = 4 * 1024 * 1024;
const REGION: RegionId = RegionId::new(0x5749_4e44_4f57_534d, 2);
const IDENTITY: &str = "EVERING_WINDOWS_IDENTITY_CHILD";
const COUNT: &str = "EVERING_WINDOWS_COUNT_CHILD";
const READY: &str = "EVERING_WINDOWS_COUNT_READY";

fn handle_count() -> u32 {
    let mut count = 0;
    assert_ne!(
        unsafe { GetProcessHandleCount(GetCurrentProcess(), &mut count) },
        0
    );
    count
}

#[test]
fn child_accepts_section_and_directional_events() {
    if let Some(name) = env::var_os(CHILD) {
        let socket = Socket::connect(name.to_str().unwrap()).unwrap();
        let offer = socket.recv(3).unwrap();
        assert_eq!(offer.bootstrap().as_ref(), b"selected-protocol-id");
        let (_, resources) = offer.into_parts();
        let mut resources = resources.into_vec();
        let section = Section::from_owned_handle(resources.remove(0));
        let session = SessionBy::<()>::open(
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
        return;
    }

    let listener = Listener::bind().unwrap();
    let mut command = Command::new(env::current_exe().unwrap());
    command
        .args(["--exact", "child_accepts_section_and_directional_events"])
        .env(CHILD, listener.name());
    let mut child = Supervisor::spawn(&mut command).unwrap();
    let socket = listener.accept(&child).unwrap();

    let section = Section::anonymous(SIZE, Access::READ | Access::WRITE).unwrap();
    let session = SessionBy::<()>::create(
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

#[test]
fn control_pipe_rejects_a_different_child_instance() {
    if let Some(name) = env::var_os(IDENTITY) {
        if name == "unrelated" {
            loop {
                thread::sleep(Duration::from_secs(1));
            }
        }
        let _socket = Socket::connect(name.to_str().unwrap()).unwrap();
        thread::sleep(Duration::from_secs(1));
        return;
    }

    let listener = Listener::bind().unwrap();
    let mut connecting = Command::new(env::current_exe().unwrap());
    connecting
        .args(["--exact", "control_pipe_rejects_a_different_child_instance"])
        .env(IDENTITY, listener.name());
    let connecting = Supervisor::spawn(&mut connecting).unwrap();

    let mut unrelated = Command::new(env::current_exe().unwrap());
    unrelated
        .args(["--exact", "control_pipe_rejects_a_different_child_instance"])
        .env(IDENTITY, "unrelated");
    let unrelated = Supervisor::spawn(&mut unrelated).unwrap();

    assert_eq!(
        listener.accept(&unrelated).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    drop(connecting);
    drop(unrelated);
}

#[test]
fn count_mismatch_closes_every_remote_handle() {
    if let Some(name) = env::var_os(COUNT) {
        let socket = Socket::connect(name.to_str().unwrap()).unwrap();
        let before = handle_count();
        std::fs::File::create(env::var_os(READY).unwrap()).unwrap();
        assert_eq!(
            socket.recv(2).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        assert_eq!(handle_count(), before);
        return;
    }

    let ready = env::temp_dir().join(format!("evering-exchange-ready-{}", std::process::id()));
    let _ = std::fs::remove_file(&ready);
    let listener = Listener::bind().unwrap();
    let mut command = Command::new(env::current_exe().unwrap());
    command
        .args(["--exact", "count_mismatch_closes_every_remote_handle"])
        .env(COUNT, listener.name())
        .env(READY, &ready);
    let mut child = Supervisor::spawn(&mut command).unwrap();
    let socket = listener.accept(&child).unwrap();
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
