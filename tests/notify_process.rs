#![cfg(all(feature = "tokio", feature = "notify", any(unix, windows)))]

use std::{env, process::Command, time::Duration};

use evering::{
    notify::{Notify, Wait as _},
    os, runtime,
};

const CHILD: &str = "EVERING_NOTIFY_CHILD";
const HANDLE: &str = "EVERING_NOTIFY_HANDLE";
const EXIT_CHILD: &str = "EVERING_NOTIFY_EXIT_CHILD";

#[test]
fn wait_cross_process() {
    if env::var_os(CHILD).is_some() {
        let raw: usize = env::var(HANDLE).unwrap().parse().unwrap();

        #[cfg(unix)]
        let event = {
            use std::os::fd::{FromRawFd, OwnedFd};
            unsafe { os::Event::from_owned_fd(OwnedFd::from_raw_fd(raw as i32)) }
        };
        #[cfg(windows)]
        let event = unsafe { os::Event::from_owned_handle(raw as *mut _) };

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                let wait = runtime::Wait::new(event).unwrap();
                wait.wait().await.unwrap();
            });
        return;
    }

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

    let mut child = Command::new(env::current_exe().unwrap())
        .arg("--exact")
        .arg("wait_cross_process")
        .arg("--nocapture")
        .env(CHILD, "1")
        .env(HANDLE, raw.to_string())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(20));
    ring.notify().unwrap();
    assert!(child.wait().unwrap().success());
}

#[test]
fn peer_exit_cancels_wait_without_fabricating_readiness() {
    if env::var_os(EXIT_CHILD).is_some() {
        return;
    }

    let (_ring, event) = os::event().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let entered = runtime.enter();
    let wait = runtime::Wait::new(event).unwrap();
    drop(entered);

    let mut child = Command::new(env::current_exe().unwrap())
        .arg("--exact")
        .arg("peer_exit_cancels_wait_without_fabricating_readiness")
        .arg("--nocapture")
        .env(EXIT_CHILD, "1")
        .spawn()
        .unwrap();
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
