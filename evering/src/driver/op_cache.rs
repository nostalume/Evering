pub mod locked {
    use core::{
        mem,
        task::{Context, Poll, Waker},
    };

    #[derive(Debug)]
    pub enum Cachestate<T> {
        Waiting(Waker),
        Completed(T),
    }

    impl<T> Default for Cachestate<T> {
        fn default() -> Self {
            Self::init()
        }
    }

    impl<T> Cachestate<T> {
        pub fn init() -> Self {
            Cachestate::Waiting(Waker::noop().clone())
        }

        pub fn try_complete(&mut self, completed: T) -> bool {
            match mem::replace(self, Cachestate::Completed(completed)) {
                Cachestate::Waiting(waker) => {
                    waker.wake();
                    true
                }
                Cachestate::Completed(_) => false,
            }
        }

        pub fn try_poll(&mut self, cx: &mut Context<'_>) -> Poll<T> {
            match self {
                Cachestate::Completed(_) => {
                    let Cachestate::Completed(payload) = mem::take(self) else {
                        unreachable!()
                    };
                    Poll::Ready(payload)
                }
                Cachestate::Waiting(waker) => {
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
        payload: UnsafeCell<MaybeUninit<T>>,
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
                payload: UnsafeCell::new(MaybeUninit::uninit()),
            }
        }

        pub fn clean(&mut self) {
            let state = *self.state.get_mut();
            if state == COMPLETED {
                unsafe {
                    (*self.payload.get()).assume_init_drop();
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
            unsafe { (*self.payload.get()).write(payload) };
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
                        if let Ok(_) = self.state.compare_exchange(
                            INIT,
                            WAITING,
                            core::sync::atomic::Ordering::AcqRel,
                            core::sync::atomic::Ordering::Acquire,
                        ) {
                            return Poll::Pending;
                        }
                    }
                    WAITING => {
                        let waker = unsafe { (*self.waker.get()).assume_init_read() };
                        if !waker.will_wake(cx.waker()) {
                            unsafe { (*self.waker.get()).write(cx.waker().clone()) };
                        }
                        if let Ok(_) = self.state.compare_exchange(
                            WAITING,
                            WAITING,
                            core::sync::atomic::Ordering::AcqRel,
                            core::sync::atomic::Ordering::Acquire,
                        ) {
                            return Poll::Pending;
                        }
                    }
                    COMPLETED => {
                        let payload = unsafe { (*self.payload.get()).assume_init_read() };
                        return Poll::Ready(payload);
                    }
                    _ => unreachable!(),
                }
            }
        }
    }
}
