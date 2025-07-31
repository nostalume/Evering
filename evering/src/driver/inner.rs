use alloc::sync::Arc;
use lock_api::Mutex;
use slab::Slab;

use core::{
    mem,
    ops::{Deref, DerefMut},
    pin::Pin,
    task::{Context, Poll, Waker},
};

use crate::{driver::DriverSpec, uring::UringSpec};

pub type OpId = usize;
type OpSigns<T> = Slab<OpSign<T>>;

pub struct OpSign<T> {
    state: OpSignState<T>,
}

pub enum OpSignState<T> {
    Waiting(Waker),
    Completed(T),
}

impl<T> OpSign<T> {
    pub fn init() -> Self {
        OpSign {
            state: OpSignState::Waiting(Waker::noop().clone()),
        }
    }

    pub fn complete(&mut self, completed: T) {
        match mem::replace(&mut self.state, OpSignState::Completed(completed)) {
            OpSignState::Waiting(waker) => waker.wake(),
            OpSignState::Completed(_) => (),
        }
    }
}

pub struct DriverCore<S: UringSpec> {
    pub ops: OpSigns<S::CQE>,
}

impl<S: UringSpec> Default for DriverCore<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: UringSpec> DriverCore<S> {
    pub fn insert(&mut self) -> OpId {
        self.ops.insert(OpSign::init())
    }

    pub fn new() -> Self {
        DriverCore {
            ops: OpSigns::new(),
        }
    }

    pub fn new_with_cap(cap: usize) -> Self {
        DriverCore {
            ops: OpSigns::with_capacity(cap),
        }
    }
}

impl<S: UringSpec> Deref for DriverCore<S> {
    type Target = OpSigns<S::CQE>;

    fn deref(&self) -> &Self::Target {
        &self.ops
    }
}
impl<S: UringSpec> DerefMut for DriverCore<S> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.ops
    }
}

impl<S: UringSpec> Drop for DriverCore<S> {
    fn drop(&mut self) {
        for op_sign in self.drain() {
            match op_sign.state {
                OpSignState::Completed(_) => {}
                OpSignState::Waiting(_) => {
                    panic!("[driver]: unhandled waiting op");
                }
            }
        }
    }
}
pub struct Driver<S: UringSpec, D: DriverSpec> {
    pub inner: Arc<Mutex<D::Lock, DriverCore<S>>>,
}

impl<S: UringSpec, D: DriverSpec> Default for Driver<S, D> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S: UringSpec, D: DriverSpec> Driver<S, D> {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(DriverCore::new())),
        }
    }

    pub fn new_with_cap(cap: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(DriverCore::new_with_cap(cap))),
        }
    }
}

impl<S: UringSpec, D: DriverSpec> Deref for Driver<S, D> {
    type Target = Arc<Mutex<D::Lock, DriverCore<S>>>;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<S: UringSpec, D: DriverSpec> Clone for Driver<S, D> {
    fn clone(&self) -> Self {
        let inner = self.inner.clone();
        Self { inner }
    }
}

pub struct Op<S: UringSpec, D: DriverSpec> {
    pub id: OpId,
    pub driver: Driver<S, D>,
}

impl<S: UringSpec, D: DriverSpec> Future for Op<S, D> {
    type Output = S::CQE;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut core = self.driver.lock();
        let Some(op_sign) = core.ops.get_mut(self.id) else {
            return Poll::Pending;
        };

        match &mut op_sign.state {
            OpSignState::Completed(_) => {
                let old_state = mem::replace(
                    &mut op_sign.state,
                    OpSignState::Waiting(Waker::noop().clone()),
                );
                let OpSignState::Completed(payload) = old_state else {
                    unreachable!();
                };
                core.remove(self.id);
                Poll::Ready(payload)
            }
            OpSignState::Waiting(waker) => {
                if !waker.will_wake(cx.waker()) {
                    *waker = cx.waker().clone();
                }
                Poll::Pending
            }
        }
    }
}

impl<S: UringSpec, D: DriverSpec> Drop for Op<S, D> {
    fn drop(&mut self) {
        let mut core = self.driver.lock();
        core.ops.try_remove(self.id);
    }
}
