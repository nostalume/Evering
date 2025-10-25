use alloc::sync::Arc;
use lock_api::Mutex;
use lock_api::RawMutex;
use slab::Slab;

use core::{
    ops::{Deref, DerefMut},
    pin::Pin,
    task::{Context, Poll},
};

use crate::driver::locked::lock::ParkMutex;
use crate::{driver::Driver, driver::cache::locked::CacheState};

pub mod lock {
    pub type SpinMutex = spin::Mutex<()>;
    pub type ParkMutex = parking_lot::RawMutex;
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

pub struct SlabDriver<T, L: RawMutex = ParkMutex> {
    inner: Arc<Mutex<L, SlabDriverCore<T>>>,
}

impl<T, L: RawMutex> Default for SlabDriver<T, L> {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(SlabDriverCore::default())),
        }
    }
}

impl<T, L: RawMutex> Clone for SlabDriver<T, L> {
    fn clone(&self) -> Self {
        let inner = self.inner.clone();
        Self { inner }
    }
}

impl<T, L: RawMutex> Driver for SlabDriver<T, L> {
    type Id = OpId;
    type Op = Op<T, L>;
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

pub struct Op<T, L: RawMutex> {
    id: OpId,
    driver: SlabDriver<T, L>,
}

impl<T, L: RawMutex> Future for Op<T, L> {
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

impl<T, L: RawMutex> Drop for Op<T, L> {
    fn drop(&mut self) {
        let mut core = self.driver.inner.lock();
        core.ops.try_remove(self.id);
    }
}
