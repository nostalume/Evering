//! Publishes `Wait` and owns only process-local registrations: it turns an `os::Event` into something a
//! Tokio task can park on, so that a caller never has to spend a thread per region, and on Linux it does
//! the same for a child's death witness, which is a descriptor of exactly the same shape held by a
//! different lane. Nothing here touches shared bytes, and the whole file exists only under the `tokio`
//! feature. Readiness is consumed inside `wait` and nowhere else — on unix by clearing the event
//! before the registration is cleared, on Windows by a wait registered once and unregistered blocking.
//! A `pidfd` is the one readiness that cannot be consumed, and that is not an oversight: an exited
//! process stays exited until its owner reaps it, so the witness is ready again on purpose.
//!
//! The Windows leg hands one allocation between two owners: the wake callback reclaims the pointer it
//! was passed, and `Drop` reclaims it only after reading, with acquire ordering, that no callback ever
//! started. That store/load pair is the whole ownership argument, and nothing counts the allocation to
//! confirm it.
#[cfg(unix)]
mod imp {
    use std::io;
    #[cfg(target_os = "linux")]
    use std::{
        os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd},
        sync::Arc,
    };

    use tokio::io::unix::AsyncFd;

    #[cfg(target_os = "linux")]
    use crate::process::Exited;
    use crate::{notify::Wait as WaitFor, os::Event};

    pub struct Wait(AsyncFd<Event>);

    impl Wait {
        pub fn new(event: Event) -> io::Result<Self> {
            AsyncFd::new(event).map(Self)
        }
    }

    impl WaitFor for Wait {
        type Error = io::Error;

        async fn wait(&self) -> Result<(), Self::Error> {
            let mut ready = self.0.readable().await?;
            self.0.get_ref().clear().map_err(io::Error::from)?;
            ready.clear_ready();
            Ok(())
        }
    }

    /// One wait's hold on a child's witness descriptor.
    ///
    /// The registration is per call rather than stored on the witness, because `Exited` belongs to the
    /// `process` lane and this file is the only one that may name a runtime. The cost is the two driver
    /// calls that add and drop the descriptor, which is what a caller pays once per death it waits for.
    #[cfg(target_os = "linux")]
    #[derive(Debug)]
    struct Registered(Arc<OwnedFd>);

    #[cfg(target_os = "linux")]
    impl AsFd for Registered {
        fn as_fd(&self) -> BorrowedFd<'_> {
            self.0.as_fd()
        }
    }

    #[cfg(target_os = "linux")]
    impl AsRawFd for Registered {
        fn as_raw_fd(&self) -> RawFd {
            self.0.as_raw_fd()
        }
    }

    /// Parks on the death of the exact child a supervisor owns, without reaping it.
    ///
    /// Unlike a doorbell, this readiness cannot be consumed: an exited process stays exited until its
    /// owner waits on it, so a second `wait` answers at once rather than parking again. What the caller
    /// still has to do is reap — the status is the supervisor's, and this reports only that the child is
    /// gone. A witness taken after the status was already recorded has no descriptor to park on and is
    /// ready immediately, which is the one case where no host object is consulted.
    #[cfg(target_os = "linux")]
    impl WaitFor for Exited {
        type Error = io::Error;

        async fn wait(&self) -> Result<(), Self::Error> {
            let Some(fd) = self.descriptor() else {
                return Ok(());
            };
            let death = AsyncFd::new(Registered(fd.clone()))?;
            let mut ready = death.readable().await?;
            ready.clear_ready();
            Ok(())
        }
    }
}

#[cfg(windows)]
mod imp {
    use core::{
        ffi::c_void,
        future::Future,
        pin::Pin,
        ptr,
        sync::atomic::{AtomicBool, Ordering},
        task::{Context, Poll, Waker},
    };
    use std::{
        io,
        sync::{Arc, Mutex},
    };
    use windows_sys::Win32::{
        Foundation::{HANDLE, INVALID_HANDLE_VALUE},
        System::Threading::{
            INFINITE, RegisterWaitForSingleObject, UnregisterWaitEx, WT_EXECUTEONLYONCE,
        },
    };

    use crate::{notify::Wait as WaitFor, os::Event};

    pub struct Wait(Event);

    impl Wait {
        pub fn new(event: Event) -> io::Result<Self> {
            Ok(Self(event))
        }
    }

    struct State {
        fired: AtomicBool,
        started: AtomicBool,
        waker: Mutex<Option<Waker>>,
    }

    unsafe extern "system" fn wake(context: *mut c_void, _: bool) {
        let state = unsafe { Arc::from_raw(context.cast::<State>()) };
        state.started.store(true, Ordering::Release);
        state.fired.store(true, Ordering::Release);
        if let Some(waker) = state.waker.lock().unwrap().take() {
            waker.wake();
        }
    }

    pub struct Ready<'a> {
        _event: &'a Event,
        state: Arc<State>,
        raw: *const State,
        wait: HANDLE,
        error: Option<io::Error>,
    }

    // Both raw values are opaque identities owned by `state`/the Windows wait
    // registration. Moving the future does not dereference either value, and
    // cancellation synchronously unregisters the callback before reclaiming
    // its raw Arc owner.
    unsafe impl Send for Ready<'_> {}

    impl<'a> Ready<'a> {
        fn new(event: &'a Event) -> Self {
            let state = Arc::new(State {
                fired: AtomicBool::new(false),
                started: AtomicBool::new(false),
                waker: Mutex::new(None),
            });
            let raw = Arc::into_raw(state.clone());
            let mut wait = ptr::null_mut();
            let registered = unsafe {
                RegisterWaitForSingleObject(
                    &mut wait,
                    event.handle(),
                    Some(wake),
                    raw.cast(),
                    INFINITE,
                    WT_EXECUTEONLYONCE,
                )
            };
            let error = if registered == 0 {
                unsafe {
                    drop(Arc::from_raw(raw));
                }
                Some(io::Error::last_os_error())
            } else {
                None
            };
            Self {
                _event: event,
                state,
                raw,
                wait,
                error,
            }
        }
    }

    impl Future for Ready<'_> {
        type Output = io::Result<()>;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            if let Some(error) = self.error.take() {
                return Poll::Ready(Err(error));
            }
            if self.state.fired.load(Ordering::Acquire) {
                return Poll::Ready(Ok(()));
            }
            *self.state.waker.lock().unwrap() = Some(cx.waker().clone());
            if self.state.fired.load(Ordering::Acquire) {
                Poll::Ready(Ok(()))
            } else {
                Poll::Pending
            }
        }
    }

    impl Drop for Ready<'_> {
        fn drop(&mut self) {
            if self.wait.is_null() {
                return;
            }
            unsafe {
                UnregisterWaitEx(self.wait, INVALID_HANDLE_VALUE);
                if !self.state.started.load(Ordering::Acquire) {
                    drop(Arc::from_raw(self.raw));
                }
            }
        }
    }

    impl WaitFor for Wait {
        type Error = io::Error;

        async fn wait(&self) -> Result<(), Self::Error> {
            Ready::new(&self.0).await?;
            self.0.clear()
        }
    }
}

#[cfg(any(unix, windows))]
pub use imp::Wait;
