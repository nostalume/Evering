#![cfg(all(feature = "tokio", feature = "notify", any(unix, windows)))]

use std::{env, time::Duration};

use evering::{
    notify::{Notify, Wait as _},
    os, runtime,
};

mod support;
use crate::support::role;

/// The doorbell the parent names to its child, spelled as the number a second process can rebuild.
const HANDLE: &str = "EVERING_NOTIFY_HANDLE";

/// The role the cross-process wait spawns: park on the handle the parent named, and leave when it rings.
fn parks() -> i32 {
    let raw: usize = env::var(HANDLE).unwrap().parse().unwrap();

    #[cfg(unix)]
    let event = {
        use std::os::fd::{FromRawFd, OwnedFd};
        os::Event::from_owned_fd(unsafe { OwnedFd::from_raw_fd(raw as i32) })
    };
    #[cfg(windows)]
    let event = {
        use std::os::windows::io::{FromRawHandle, OwnedHandle};
        os::Event::from_owned_handle(unsafe { OwnedHandle::from_raw_handle(raw as *mut _) })
    };

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async move {
            let wait = runtime::Wait::new(event).unwrap();
            wait.wait().await.unwrap();
        });
    0
}

/// The role the peer-exit claim spawns: leave at once, so a death is the only answer the wait can get.
fn leaves() -> i32 {
    0
}

/// The child's whole program: the roles this claim defines, and nothing else.
#[test]
fn role_entry() {
    role::dispatch(&[("parks", parks), ("leaves", leaves)]);
}

#[test]
fn wait_cross_process() {
    let (ring, event) = os::event().unwrap();

    #[cfg(unix)]
    let raw = {
        use std::os::fd::{AsFd, AsRawFd};
        nix::fcntl::fcntl(
            event.as_fd(),
            nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::empty()),
        )
        .unwrap();
        event.as_raw_fd() as usize
    };
    #[cfg(windows)]
    let raw = {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
        let handle = event.as_raw_handle();
        assert_ne!(
            unsafe {
                SetHandleInformation(handle.cast(), HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT)
            },
            0
        );
        handle as usize
    };

    let mut child = role::child("parks")
        .env(HANDLE, raw.to_string())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(20));
    ring.notify().unwrap();
    assert!(child.wait().unwrap().success());
}

#[test]
fn peer_exit_cancels_wait_without_fabricating_readiness() {
    let (_ring, event) = os::event().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let entered = runtime.enter();
    let wait = runtime::Wait::new(event).unwrap();
    drop(entered);

    let mut child = role::child("leaves").spawn().unwrap();
    let (exited, status) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let _ = exited.send(child.wait());
    });

    runtime.block_on(async {
        tokio::select! {
            result = wait.wait() => panic!("peer exit fabricated readiness: {result:?}"),
            result = status => assert!(result.unwrap().unwrap().success()),
        }
    });
}
