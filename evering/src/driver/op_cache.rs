pub mod locked {
    use core::{
        mem,
        task::{Context, Poll, Waker},
    };

    #[derive(Debug)]
    pub enum CacheState<T> {
        Waiting(Waker),
        Completed(T),
    }

    impl<T> Default for CacheState<T> {
        fn default() -> Self {
            Self::init()
        }
    }

    impl<T> CacheState<T> {
        pub fn init() -> Self {
            CacheState::Waiting(Waker::noop().clone())
        }

        pub fn try_complete(&mut self, completed: T) -> bool {
            match mem::replace(self, CacheState::Completed(completed)) {
                CacheState::Waiting(waker) => {
                    waker.wake();
                    true
                }
                CacheState::Completed(_) => false,
            }
        }

        pub fn try_poll(&mut self, cx: &mut Context<'_>) -> Poll<T> {
            match self {
                CacheState::Completed(_) => {
                    let CacheState::Completed(payload) = mem::take(self) else {
                        unreachable!()
                    };
                    Poll::Ready(payload)
                }
                CacheState::Waiting(waker) => {
                    if !waker.will_wake(cx.waker()) {
                        *waker = cx.waker().clone();
                    }
                    Poll::Pending
                }
            }
        }

        pub fn clean(&mut self) {
            mem::take(self);
        }
    }
}

pub mod unlocked {
    use core::{
        cell::UnsafeCell,
        mem::MaybeUninit,
        sync::atomic::AtomicU8,
        task::{Context, Poll, Waker},
    };

    const INIT: u8 = 0;
    const WAITING: u8 = 1;
    const COMPLETED: u8 = 2;

    pub struct CacheState<T> {
        state: AtomicU8,
        waker: UnsafeCell<MaybeUninit<Waker>>,
        // take rather read to move out, avoiding free after read.
        payload: UnsafeCell<Option<T>>,
    }

    unsafe impl<T: Send> Send for CacheState<T> {}
    unsafe impl<T: Sync> Sync for CacheState<T> {}

    impl<T> Default for CacheState<T> {
        fn default() -> Self {
            Self::init()
        }
    }

    impl<T> CacheState<T> {
        pub fn init() -> Self {
            Self {
                state: AtomicU8::new(INIT),
                waker: UnsafeCell::new(MaybeUninit::uninit()),
                payload: UnsafeCell::new(None),
            }
        }

        pub fn clean(&mut self) {
            let state = *self.state.get_mut();
            if state == COMPLETED {
                unsafe {
                    (*self.payload.get()).take();
                }
            }
            if state == WAITING {
                unsafe {
                    (*self.waker.get()).assume_init_drop();
                }
            }

            *self.state.get_mut() = INIT;
        }

        pub fn try_complete(&self, payload: T) {
            unsafe { (*self.payload.get()).replace(payload) };
            if self
                .state
                .swap(COMPLETED, core::sync::atomic::Ordering::AcqRel)
                == WAITING
            {
                let waker = unsafe { (*self.waker.get()).assume_init_read() };
                waker.wake();
            }
        }

        pub fn try_poll(&self, cx: &mut Context<'_>) -> Poll<T> {
            loop {
                match self.state.load(core::sync::atomic::Ordering::Acquire) {
                    INIT => {
                        unsafe { (*self.waker.get()).write(cx.waker().clone()) };
                        if self
                            .state
                            .compare_exchange(
                                INIT,
                                WAITING,
                                core::sync::atomic::Ordering::AcqRel,
                                core::sync::atomic::Ordering::Acquire,
                            )
                            .is_ok()
                        {
                            return Poll::Pending;
                        }
                    }
                    WAITING => {
                        let waker = unsafe { (*self.waker.get()).assume_init_read() };
                        if !waker.will_wake(cx.waker()) {
                            unsafe { (*self.waker.get()).write(cx.waker().clone()) };
                        }
                        if self
                            .state
                            .compare_exchange(
                                WAITING,
                                WAITING,
                                core::sync::atomic::Ordering::AcqRel,
                                core::sync::atomic::Ordering::Acquire,
                            )
                            .is_ok()
                        {
                            return Poll::Pending;
                        }
                    }
                    COMPLETED => {
                        let payload = unsafe { (*self.payload.get()).take().unwrap() };
                        return Poll::Ready(payload);
                    }
                    _ => unreachable!(),
                }
            }
        }
    }
}
