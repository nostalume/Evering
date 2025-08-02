use alloc::sync::Arc;
use lock_api::Mutex;
use lock_api::RawMutex;
use slab::Slab;

use core::{
    mem,
    ops::{Deref, DerefMut},
    pin::Pin,
    task::{Context, Poll},
};

use crate::{
    driver::Driver,
    driver::op_cache::{OpCache, OpCacheState},
    uring::UringSpec,
};

pub trait LockDriverSpec {
    type Lock: RawMutex;
    // ...
}

pub mod lock {
    pub type SpinMutex = spin::Mutex<()>;

    #[cfg(feature = "std")]
    pub type StdMutex = parking_lot::RawMutex;
}

type OpId = usize;
type OpSigns<T> = Slab<OpCache<T>>;

#[derive(Debug)]
struct SlabDriverCore<T> {
    ops: OpSigns<T>,
}

impl<T> SlabDriverCore<T> {
    pub fn new_with_cap(cap: usize) -> Self {
        SlabDriverCore {
            ops: OpSigns::with_capacity(cap),
        }
    }

    pub fn insert(&mut self) -> OpId {
        self.ops.insert(OpCache::init())
    }
}

impl<T> Default for SlabDriverCore<T> {
    fn default() -> Self {
        SlabDriverCore {
            ops: OpSigns::new(),
        }
    }
}

impl<T> Deref for SlabDriverCore<T> {
    type Target = OpSigns<T>;

    fn deref(&self) -> &Self::Target {
        &self.ops
    }
}
impl<T> DerefMut for SlabDriverCore<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.ops
    }
}

impl<T> Drop for SlabDriverCore<T> {
    fn drop(&mut self) {
        for op_sign in self.drain() {
            match op_sign.state {
                OpCacheState::Completed(_) => {}
                OpCacheState::Waiting(_) => {
                    panic!("[driver]: unhandled waiting op");
                }
            }
        }
    }
}

pub struct SlabDriver<S: UringSpec, D: LockDriverSpec> {
    inner: Arc<Mutex<D::Lock, SlabDriverCore<S::CQE>>>,
}

impl<S: UringSpec, D: LockDriverSpec> Default for SlabDriver<S, D> {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(SlabDriverCore::default())),
        }
    }
}

impl<S: UringSpec, D: LockDriverSpec> Clone for SlabDriver<S, D> {
    fn clone(&self) -> Self {
        let inner = self.inner.clone();
        Self { inner }
    }
}

impl<S:UringSpec, D: LockDriverSpec> UringSpec for SlabDriver<S, D> {
    type SQE = S::SQE;
    type CQE = S::CQE;
}

impl<S: UringSpec, D: LockDriverSpec> Driver for SlabDriver<S, D> {
    type Id = OpId;
    type Op = Op<S, D>;
    type Config = usize;

    fn new(cap: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(SlabDriverCore::new_with_cap(cap))),
        }
    }

    fn register(&self) -> (Self::Id, Self::Op) {
        let id = self.inner.lock().insert();
        let op = Op {
            id,
            driver: self.clone(),
        };

        (id, op)
    }

    fn complete(&self, id: Self::Id, payload: <Self::Op as Future>::Output) {
        let mut core = self.inner.lock();
        // if the op is dropped, `op_sign` won't exist.
        if let Some(op_sign) = core.get_mut(id) {
            op_sign.complete(payload);
        }
    }
}

pub struct Op<S: UringSpec, D: LockDriverSpec> {
    id: OpId,
    driver: SlabDriver<S, D>,
}

impl<S: UringSpec, D: LockDriverSpec> Future for Op<S, D> {
    type Output = S::CQE;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut core = self.driver.inner.lock();
        let Some(op_sign) = core.ops.get_mut(self.id) else {
            return Poll::Pending;
        };

        match &mut op_sign.state {
            OpCacheState::Completed(_) => {
                let OpCacheState::Completed(payload) = mem::take(&mut op_sign.state) else {
                    unreachable!()
                };
                core.remove(self.id);
                Poll::Ready(payload)
            }
            OpCacheState::Waiting(waker) => {
                if !waker.will_wake(cx.waker()) {
                    *waker = cx.waker().clone();
                }
                Poll::Pending
            }
        }
    }
}

impl<S: UringSpec, D: LockDriverSpec> Drop for Op<S, D> {
    fn drop(&mut self) {
        let mut core = self.driver.inner.lock();
        core.ops.try_remove(self.id);
    }
}
