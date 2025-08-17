use core::marker::PhantomData;

use crate::{seal::Sealed, uring::UringSpec};

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

pub struct DriverUring<D: Driver> {
    _marker: PhantomData<D>,
}

impl<D: Driver> UringSpec for DriverUring<D> {
    type SQE = cell::IdCell<D::Id, D::SQE>;
    type CQE = cell::IdCell<D::Id, D::CQE>;
}

pub trait Role: Sealed {}

pub struct Submit;
impl Sealed for Submit {}
impl Role for Submit {}
pub struct Receive;
impl Sealed for Receive {}
impl Role for Receive {}

pub struct BridgeTmpl<D: Driver, T: Clone, R: Role> {
    driver: D,
    sq: T,
    _marker: PhantomData<R>,
}

impl<D: Driver, T: Clone, R: Role> Clone for BridgeTmpl<D, T, R> {
    fn clone(&self) -> Self {
        Self {
            driver: self.driver.clone(),
            sq: self.sq.clone(),
            _marker: PhantomData,
        }
    }
}

pub mod asynch {
    use core::marker::PhantomData;

    use crate::driver::{BridgeTmpl, Receive, Submit};
    use crate::uring::UringSpec;
    use crate::{
        driver::{Driver, DriverUring, cell},
        uring::asynch::{
            Completer as UringCompleter, Submitter as UringSubbmiter, channel, default_channel,
        },
    };
    use async_channel::{SendError, TrySendError};

    pub type Submitter<D> = UringSubbmiter<DriverUring<D>>;
    pub type Completer<D> = UringCompleter<DriverUring<D>>;

    pub type Bridge<D, R> = BridgeTmpl<D, Submitter<D>, R>;
    pub type SubmitBridge<D> = Bridge<D, Submit>;
    pub type ReceiveBridge<D> = Bridge<D, Receive>;

    type UringBridge<D> = (SubmitBridge<D>, ReceiveBridge<D>, Completer<D>);

    pub fn new<D: Driver>(uring_cap: usize, driver_cfg: D::Config) -> UringBridge<D> {
        let (cq, sq) = channel(uring_cap);
        let d = D::new(driver_cfg);
        let sb = BridgeTmpl {
            driver: d.clone(),
            sq: sq.clone(),
            _marker: PhantomData,
        };
        let cb = BridgeTmpl {
            driver: d,
            sq,
            _marker: PhantomData,
        };
        (sb, cb, cq)
    }

    pub fn default<D: Driver>() -> UringBridge<D> {
        let (cq, sq) = default_channel();
        let d = D::default();
        let sb = BridgeTmpl {
            driver: d.clone(),
            sq: sq.clone(),
            _marker: PhantomData,
        };
        let cb = BridgeTmpl {
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
}

pub mod bare {
    use core::marker::PhantomData;

    use evering_shm::shm_alloc::ShmAllocator;

    use crate::driver::{BridgeTmpl, Driver, DriverUring, Receive, Submit};
    use crate::uring::UringSpec;
    use crate::uring::bare::{
        box_channel, channel, Boxed, BoxedQueue, Completer as UringCompleter, Submitter as UringSubbmiter
    };

    pub type Submitter<D, P, const N: usize> = UringSubbmiter<DriverUring<D>, P, N>;
    pub type Completer<D, P, const N: usize> = UringCompleter<DriverUring<D>, P, N>;

    pub type Bridge<D, P, const N: usize, R> = BridgeTmpl<D, Submitter<D, P, N>, R>;
    pub type SubmitBridge<D, P, const N: usize> = Bridge<D, P, N, Submit>;
    pub type ReceiveBridge<D, P, const N: usize> = Bridge<D, P, N, Receive>;

    pub type UringBridge<D, P, const N: usize> = (
        SubmitBridge<D, P, N>,
        ReceiveBridge<D, P, N>,
        Completer<D, P, N>,
    );
    pub type OwnUringBridge<D, const N: usize> = (
        SubmitBridge<D, (), N>,
        ReceiveBridge<D, (), N>,
        Completer<D, (), N>,
    );
    pub type BoxUringBridge<D, A, const N: usize> = (
        SubmitBridge<D, Boxed<A>, N>,
        ReceiveBridge<D, Boxed<A>, N>,
        Completer<D, Boxed<A>, N>,
    );

    pub fn own_new<D: Driver, const N: usize>(driver_cfg: D::Config) -> OwnUringBridge<D, N> {
        let (sq, cq) = channel::<DriverUring<D>, N>();
        let d = D::new(driver_cfg);
        let sb = BridgeTmpl {
            driver: d.clone(),
            sq: sq.clone(),
            _marker: PhantomData,
        };
        let cb = BridgeTmpl {
            driver: d,
            sq,
            _marker: PhantomData,
        };
        (sb, cb, cq)
    }

    pub fn box_new<D: Driver, A: ShmAllocator, const N: usize>(
        q: (
            BoxedQueue<<DriverUring<D> as UringSpec>::SQE, A, N>,
            BoxedQueue<<DriverUring<D> as UringSpec>::CQE, A, N>,
        ),
        driver_cfg: D::Config,
    ) -> BoxUringBridge<D, A, N> {
        let (sq, cq) = box_channel(q.0, q.1);
        let d = D::new(driver_cfg);
        let sb = BridgeTmpl {
            driver: d.clone(),
            sq: sq.clone(),
            _marker: PhantomData,
        };
        let cb = BridgeTmpl {
            driver: d,
            sq,
            _marker: PhantomData,
        };
        (sb, cb, cq)
    }
}
