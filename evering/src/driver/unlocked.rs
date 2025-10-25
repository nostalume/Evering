use core::{
    ops::Deref,
    pin::Pin,
    ptr::{self, NonNull},
    task::{Context, Poll},
};

use objectpool::{Pool, ReusableObject};

use crate::driver::{Driver, cache::unlocked::CacheState};

type OpCache<T> = CacheState<T>;
type OpPool<T> = Pool<OpCache<T>>;
type OpId<T> = LeakRef<ReusableObject<OpCache<T>>>;

pub struct Leak<T> {
    ptr: NonNull<T>,
}

pub struct LeakRef<T> {
    ptr: NonNull<T>,
}

unsafe impl<T: Send> Send for Leak<T> {}
unsafe impl<T: Sync> Sync for Leak<T> {}
unsafe impl<T: Send> Send for LeakRef<T> {}
unsafe impl<T: Sync> Sync for LeakRef<T> {}

impl<T> Leak<T> {
    fn new(data: T) -> Self {
        Self {
            ptr: Box::leak(Box::new(data)).into(),
        }
    }

    fn as_ref(&self) -> LeakRef<T> {
        LeakRef { ptr: self.ptr }
    }
}

impl<T> Deref for Leak<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref() }
    }
}

impl<T> Clone for Leak<T> {
    fn clone(&self) -> Self {
        Self {
            ptr: self.ptr.clone(),
        }
    }
}

impl<T> Clone for LeakRef<T> {
    fn clone(&self) -> Self {
        Self {
            ptr: self.ptr.clone(),
        }
    }
}

impl<T> Drop for Leak<T> {
    fn drop(&mut self) {
        unsafe {
            ptr::drop_in_place(&mut (*self.ptr.as_ptr()));
        }
    }
}

pub struct PoolDriver<T> {
    pool: OpPool<T>,
}

impl<T> Clone for PoolDriver<T> {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
        }
    }
}

impl<T> Default for PoolDriver<T>
where
    T: 'static,
{
    fn default() -> Self {
        Self {
            pool: Pool::unbounded(Default::default, |op_cache: &mut OpCache<T>| {
                // let mut inner = op_cache.lock();
                op_cache.clean()
            }),
        }
    }
}

impl<T> Driver for PoolDriver<T>
where
    T: 'static,
{
    type Id = LeakRef<ReusableObject<OpCache<T>>>;
    type Op = Op<T>;
    type Config = usize;
    fn new(cap: usize) -> Self {
        Self {
            pool: Pool::bounded(cap, Default::default, |op_cache: &mut OpCache<T>| {
                // let mut inner = op_cache.lock();
                op_cache.clean()
            }),
        }
    }

    fn register(&self) -> (Self::Id, Self::Op) {
        let op = Op(Leak::new(self.pool.get_owned()));
        let id = op.as_ref();

        (id, op)
    }

    fn complete(&self, id: Self::Id, payload: <Self::Op as Future>::Output) {
        let id = unsafe { id.ptr.as_ref() };
        // else reference dropped without any effect.
        if id.valid_magic() {
            id.try_complete(payload);
        }
    }
}

#[repr(transparent)]
pub struct Op<T>(Leak<ReusableObject<OpCache<T>>>);

impl<T> Op<T> {
    fn as_ref(&self) -> OpId<T> {
        self.0.as_ref()
    }
}

impl<T> Future for Op<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.0.try_poll(cx)
    }
}
