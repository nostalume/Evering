use alloc::sync::Arc;
use lock_api::Mutex;
use lock_api::RawMutex;
use slab::Slab;

use core::{
    ops::{Deref, DerefMut},
    pin::Pin,
    task::{Context, Poll},
};

use crate::{driver::Driver, driver::op_cache::locked::CacheState};

pub mod lock {
    pub type SpinMutex = spin::Mutex<()>;

    #[cfg(feature = "std")]
    pub type StdMutex = parking_lot::RawMutex;
}

pub trait LockFor {
    type Lock: RawMutex;
    // ...
}

type OpId = usize;
type OpCaches<T> = Slab<CacheState<T>>;

#[derive(Debug)]
struct SlabDriverCore<T> {
    ops: OpCaches<T>,
}

impl<T> SlabDriverCore<T> {
    pub fn new_with_cap(cap: usize) -> Self {
        SlabDriverCore {
            ops: OpCaches::with_capacity(cap),
        }
    }

    pub fn insert(&mut self) -> OpId {
        self.ops.insert(CacheState::init())
    }
}

impl<T> Default for SlabDriverCore<T> {
    fn default() -> Self {
        SlabDriverCore {
            ops: OpCaches::new(),
        }
    }
}

impl<T> Deref for SlabDriverCore<T> {
    type Target = OpCaches<T>;

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
            match op_sign {
                CacheState::Completed(_) => {}
                CacheState::Waiting(_) => {
                    panic!("[driver]: unhandled waiting op");
                }
            }
        }
    }
}

pub struct SlabDriver<T, D: LockFor> {
    inner: Arc<Mutex<D::Lock, SlabDriverCore<T>>>,
}

impl<T, D: LockFor> Default for SlabDriver<T, D> {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(SlabDriverCore::default())),
        }
    }
}

impl<T, D: LockFor> Clone for SlabDriver<T, D> {
    fn clone(&self) -> Self {
        let inner = self.inner.clone();
        Self { inner }
    }
}

impl<T, D: LockFor> Driver for SlabDriver<T, D> {
    type Id = OpId;
    type Op = Op<T, D>;
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
        if let Some(op_cache) = core.get_mut(id) {
            op_cache.try_complete(payload);
        }
    }
}

pub struct Op<T, D: LockFor> {
    id: OpId,
    driver: SlabDriver<T, D>,
}

impl<T, D: LockFor> Future for Op<T, D> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut core = self.driver.inner.lock();
        let Some(op_sign) = core.ops.get_mut(self.id) else {
            return Poll::Pending;
        };

        match op_sign.try_poll(cx) {
            Poll::Ready(payload) => {
                core.remove(self.id);
                Poll::Ready(payload)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T, D: LockFor> Drop for Op<T, D> {
    fn drop(&mut self) {
        let mut core = self.driver.inner.lock();
        core.ops.try_remove(self.id);
    }
}
