#[cfg(unix)]
mod imp {
    use core::{
        future::Future,
        pin::Pin,
        task::{Context, Poll},
    };
    use std::io;

    use tokio::io::unix::AsyncFd;

    use crate::{Listen, os::Event};

    pub struct Wait(AsyncFd<Event>);

    impl Wait {
        pub fn new(event: Event) -> io::Result<Self> {
            AsyncFd::new(event).map(Self)
        }

        pub fn into_inner(self) -> Event {
            self.0.into_inner()
        }
    }

    pub struct Ready<'a>(&'a AsyncFd<Event>);

    impl Future for Ready<'_> {
        type Output = io::Result<()>;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            match self.0.poll_read_ready(cx) {
                Poll::Ready(Ok(mut guard)) => {
                    guard.clear_ready();
                    Poll::Ready(Ok(()))
                }
                Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
                Poll::Pending => Poll::Pending,
            }
        }
    }

    impl Listen for Wait {
        type Error = io::Error;
        type Ready<'a> = Ready<'a>;

        fn ready(&self) -> Self::Ready<'_> {
            Ready(&self.0)
        }

        fn clear(&self) -> Result<(), Self::Error> {
            self.0.get_ref().clear().map_err(Into::into)
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

    use crate::{Listen, os::Event};

    pub struct Wait(Event);

    impl Wait {
        pub fn new(event: Event) -> io::Result<Self> {
            Ok(Self(event))
        }

        pub fn into_inner(self) -> Event {
            self.0
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

    impl Listen for Wait {
        type Error = io::Error;
        type Ready<'a> = Ready<'a>;

        fn ready(&self) -> Self::Ready<'_> {
            Ready::new(&self.0)
        }

        fn clear(&self) -> Result<(), Self::Error> {
            self.0.clear()
        }
    }
}

#[cfg(any(unix, windows))]
pub use imp::Wait;

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use crate::{Listen, Notify, os};

    #[tokio::test]
    async fn native_event_is_sticky_before_runtime_registration() {
        let (ring, event) = os::event().unwrap();
        ring.notify().unwrap();

        let event = super::Wait::new(event).unwrap();
        fn require_send<T: Send>(_: T) {}
        require_send(event.ready());

        event.ready().await.unwrap();
        event.clear().unwrap();
    }

    #[tokio::test]
    async fn cancelling_one_wait_does_not_consume_the_latch() {
        let (ring, event) = os::event().unwrap();

        let event = super::Wait::new(event).unwrap();

        {
            let _cancelled = event.ready();
        }
        ring.notify().unwrap();
        event.ready().await.unwrap();
        event.clear().unwrap();
    }
}
