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
        sync::atomic::{AtomicU8, AtomicU32, Ordering},
        task::{Context, Poll, Waker},
    };

    const INIT: u8 = 0;
    const WAITING: u8 = 1;
    const COMPLETED: u8 = 2;
    const IN_PROGRESS: u8 = 3; // reservation state used by completer

    #[repr(C)]
    pub struct CacheState<T> {
        magic: AtomicU32,
        state: AtomicU8,
        waker: UnsafeCell<MaybeUninit<Waker>>,
        // take rather read to move out, avoiding free after read.
        payload: UnsafeCell<MaybeUninit<T>>,
    }

    unsafe impl<T: Send> Send for CacheState<T> {}
    unsafe impl<T: Sync> Sync for CacheState<T> {}

    impl<T> core::fmt::Debug for CacheState<T> {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.debug_struct("CacheState")
                .field("state", &self.state.load(Ordering::Relaxed))
                .finish()
        }
    }

    impl<T> CacheState<T> {
        const MAGIC: u32 = 0x12312;

        #[inline]
        pub const fn new() -> Self {
            Self {
                magic: AtomicU32::new(Self::MAGIC),
                state: AtomicU8::new(INIT),
                waker: UnsafeCell::new(MaybeUninit::uninit()),
                payload: UnsafeCell::new(MaybeUninit::uninit()),
            }
        }

        pub fn valid_magic(&self) -> bool {
            self.magic.load(Ordering::Relaxed) == Self::MAGIC
        }

        unsafe fn init_waker(&self, cx: &Context<'_>) {
            unsafe {
                (*self.waker.get()).write(cx.waker().clone());
            }
        }

        unsafe fn read_waker_ref(&self) -> &Waker {
            unsafe { (*self.waker.get()).assume_init_ref() }
        }

        unsafe fn read_waker(&self) -> Waker {
            unsafe { (*self.waker.get()).assume_init_read() }
        }

        unsafe fn drop_waker(&self) {
            unsafe { (*self.waker.get()).assume_init_drop() }
        }

        unsafe fn init_payload(&self, payload: T) {
            unsafe {
                (*self.payload.get()).write(payload);
            }
        }

        unsafe fn take_payload(&self) -> T {
            unsafe {
                // **Take out the value**
                self.payload.replace(MaybeUninit::uninit()).assume_init()
            }
        }

        pub fn clean(&mut self) {
            let state = *self.state.get_mut();
            if state == COMPLETED {
                unsafe {
                    let _ = self.take_payload();
                }
            }
            if state == WAITING {
                unsafe {
                    self.drop_waker();
                }
            }

            *self.state.get_mut() = INIT;
        }

        #[inline]
        pub fn try_complete(&self, payload: T) -> bool {
            loop {
                let s = self.state.load(Ordering::Acquire);
                match s {
                    COMPLETED => {
                        drop(payload);
                        return false;
                    }
                    IN_PROGRESS => {
                        core::hint::spin_loop();
                        continue;
                    }
                    _ => {
                        if self
                            .state
                            .compare_exchange_weak(
                                s,
                                IN_PROGRESS,
                                Ordering::AcqRel,
                                Ordering::Acquire,
                            )
                            .is_ok()
                        {
                            break;
                        } else {
                            continue;
                        }
                    }
                }
            }

            unsafe {
                self.init_payload(payload);
            }
            let prev = self.state.swap(COMPLETED, Ordering::AcqRel);
            if prev == WAITING {
                let w = unsafe { self.read_waker() };
                w.wake();
            }
            true
        }

        #[inline]
        pub fn try_poll(&self, cx: &mut Context<'_>) -> Poll<T> {
            loop {
                match self.state.load(Ordering::Acquire) {
                    INIT => {
                        unsafe { (*self.waker.get()).write(cx.waker().clone()) };
                        if self
                            .state
                            .compare_exchange(INIT, WAITING, Ordering::AcqRel, Ordering::Acquire)
                            .is_ok()
                        {
                            return Poll::Pending;
                        }
                    }
                    WAITING => {
                        let waker = unsafe { self.read_waker_ref() };
                        if !waker.will_wake(cx.waker()) {
                            unsafe {
                                // replace!
                                self.init_waker(cx);
                            };
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
                        } else {
                            continue;
                        }
                    }
                    COMPLETED => {
                        let payload = unsafe { self.take_payload() };
                        return Poll::Ready(payload);
                    }
                    _ => unreachable!(),
                }
            }
        }
    }
}
