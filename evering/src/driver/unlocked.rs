use core::{
    pin::Pin,
    task::{Context, Poll},
};

use alloc::sync::Arc;
use objectpool::{Pool, ReusableObject};

use crate::{
    driver::{Driver, op_cache::unlocked::CacheState},
    uring::UringSpec,
};

type OpCache<T> = CacheState<T>;
type OpId<T> = Arc<ReusableObject<OpCache<T>>>;
type OpPool<T> = Pool<OpCache<T>>;

pub struct PoolDriver<U: UringSpec> {
    pool: OpPool<U::CQE>,
}

impl<U: UringSpec> Clone for PoolDriver<U> {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
        }
    }
}

impl<U: UringSpec> Default for PoolDriver<U>
where
    U::CQE: 'static,
{
    fn default() -> Self {
        Self {
            pool: Pool::unbounded(Default::default, |op_cache: &mut OpCache<U::CQE>| {
                // let mut inner = op_cache.lock();
                op_cache.clean()
            }),
        }
    }
}

impl<U: UringSpec> UringSpec for PoolDriver<U> {
    type SQE = U::SQE;
    type CQE = U::CQE;
}

impl<U: UringSpec> Driver for PoolDriver<U>
where
    <U as UringSpec>::CQE: 'static,
{
    type Id = OpId<U::CQE>;
    type Op = Op<U>;
    type Config = usize;
    fn new(cap: usize) -> Self {
        Self {
            pool: Pool::bounded(cap, Default::default, |op_cache: &mut OpCache<U::CQE>| {
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

pub struct Op<U: UringSpec> {
    id: OpId<U::CQE>,
}

impl<U: UringSpec> Future for Op<U> {
    type Output = U::CQE;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.id.try_poll(cx)
    }
}
