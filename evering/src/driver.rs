use alloc::alloc::{Allocator, Global};
use core::marker::PhantomData;

use async_channel::{SendError, TrySendError};

use crate::uring::UringSpec;
use crate::uring::asynch::{
    Completer as UringCompleter, Submitter as UringSubbmiter, channel, channel_in, default_channel,
    default_channel_in,
};

mod cell;
pub mod locked;
mod op_cache;
pub mod unlocked;

pub trait Driver: UringSpec + Clone + Default {
    type Id;
    type Config;
    type Op: Future<Output = Self::CQE>;
    fn register(&self) -> (Self::Id, Self::Op);
    fn complete(&self, id: Self::Id, payload: <Self::Op as Future>::Output);
    fn new(cfg: Self::Config) -> Self;
}

pub type Submitter<D, A = Global> = UringSubbmiter<DriverUring<D>, A>;
pub type Completer<D, A = Global> = UringCompleter<DriverUring<D>, A>;

pub struct DriverUring<D: Driver> {
    _marker: PhantomData<D>,
}

impl<D: Driver> UringSpec for DriverUring<D> {
    type SQE = cell::IdCell<D::Id, D::SQE>;
    type CQE = cell::IdCell<D::Id, D::CQE>;
}

pub struct Bridge<D: Driver, R: Role, A: Allocator = Global> {
    driver: D,
    sq: Submitter<D, A>,
    _marker: PhantomData<R>,
}

impl<D: Driver, R: Role, A: Allocator> Clone for Bridge<D, R, A> {
    fn clone(&self) -> Self {
        Self {
            driver: self.driver.clone(),
            sq: self.sq.clone(),
            _marker: PhantomData,
        }
    }
}

pub trait Role {}

pub struct Submit;
impl Role for Submit {}
pub struct Receive;
impl Role for Receive {}

pub type SubmitBridge<D, A> = Bridge<D, Submit, A>;
pub type CompleteBridge<D, A> = Bridge<D, Receive, A>;

type UringBridge<D, A = Global> =
    (SubmitBridge<D, A>, CompleteBridge<D, A>, Completer<D, A>);

pub fn new_in<D: Driver, A: Allocator>(
    uring_cap: usize,
    alloc: &A,
    driver_cfg: D::Config,
) -> UringBridge<D, &A> {
    let (cq, sq) = channel_in(uring_cap, alloc);
    let d = D::new(driver_cfg);
    let sb = Bridge {
        driver: d.clone(),
        sq: sq.clone(),
        _marker: PhantomData,
    };
    let cb = Bridge {
        driver: d,
        sq,
        _marker: PhantomData,
    };
    (sb, cb, cq)
}

pub fn default_in<D: Driver, A: Allocator>(alloc: &A) -> UringBridge<D, &A> {
    let (cq, sq) = default_channel_in(alloc);
    let d = D::default();
    let sb = Bridge {
        driver: d.clone(),
        sq: sq.clone(),
        _marker: PhantomData,
    };
    let cb = Bridge {
        driver: d,
        sq,
        _marker: PhantomData,
    };
    (sb, cb, cq)
}

pub fn new<D: Driver>(uring_cap: usize, driver_cfg: D::Config) -> UringBridge<D> {
    let (cq, sq) = channel(uring_cap);
    let d = D::new(driver_cfg);
    let sb = Bridge {
        driver: d.clone(),
        sq: sq.clone(),
        _marker: PhantomData,
    };
    let cb = Bridge {
        driver: d,
        sq,
        _marker: PhantomData,
    };
    (sb, cb, cq)
}

pub fn default<D: Driver>() -> UringBridge<D> {
    let (cq, sq) = default_channel();
    let d = D::default();
    let sb = Bridge {
        driver: d.clone(),
        sq: sq.clone(),
        _marker: PhantomData,
    };
    let cb = Bridge {
        driver: d,
        sq,
        _marker: PhantomData,
    };
    (sb, cb, cq)
}

impl<D: Driver> Bridge<D, Submit> {
    /// submits a request in pending blocking.
    ///
    /// If the channel is closed, it will return an error.
    pub async fn submit(
        &self,
        data: D::SQE,
    ) -> Result<D::Op, SendError<<DriverUring<D> as UringSpec>::SQE>> {
        let (id, op) = self.driver.register();
        let req = cell::IdCell::new(id, data);
        self.sq.sender().send(req).await?;

        Ok(op)
    }

    /// submits a request in non-blocking.
    ///
    /// If the channel is closed or full, it will return an error immediately.
    pub fn try_submit(
        &self,
        data: D::SQE,
    ) -> Result<D::Op, TrySendError<<DriverUring<D> as UringSpec>::SQE>> {
        let (id, op) = self.driver.register();
        let req = cell::IdCell::new(id, data);
        self.sq.sender().try_send(req)?;

        Ok(op)
    }
}

impl<D: Driver> Bridge<D, Receive> {
    /// Receives completed msgs by blocking in pending.
    ///
    /// If the channel is empty or closed, it will block in pending.
    pub async fn complete(&self) {
        while let Ok(data) = self.sq.receiver().recv().await {
            let (id, payload) = data.into_inner();
            self.driver.complete(id, payload);
        }
    }

    /// Receives completed msgs in non-blocking.
    ///
    /// If the channel is empty or closed, it will return immediately.
    pub fn try_complete(&self) {
        while let Ok(data) = self.sq.receiver().try_recv() {
            let (id, payload) = data.into_inner();
            self.driver.complete(id, payload);
        }
    }
}
