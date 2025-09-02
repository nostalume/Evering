use core::{
    pin::Pin,
    task::{Context, Poll},
};

use alloc::sync::Arc;
use objectpool::{Pool, ReusableObject};

use crate::driver::{Driver, op_cache::unlocked::CacheState};

type OpCache<T> = CacheState<T>;
type OpId<T> = Arc<ReusableObject<OpCache<T>>>;
type OpPool<T> = Pool<OpCache<T>>;

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
    type Id = OpId<T>;
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
        let id = Arc::new(self.pool.get_owned());

        (id.clone(), Op { id })
    }

    fn complete(&self, id: Self::Id, payload: <Self::Op as Future>::Output) {
        id.try_complete(payload);
    }
}

pub struct Op<T> {
    id: OpId<T>,
}

impl<T> Future for Op<T> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.id.try_poll(cx)
    }
}
