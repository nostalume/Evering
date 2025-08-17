use core::marker::PhantomData;

use crate::{
    seal::Sealed,
    uring::{IReceiver, ISender, UringSpec},
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

pub struct DUring<D: Driver> {
    _marker: PhantomData<D>,
}

impl<D: Driver> UringSpec for DUring<D> {
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

pub struct BridgeTmpl<D: Driver, T: ISender + IReceiver + Clone, R: Role> {
    driver: D,
    sq: T,
    _marker: PhantomData<R>,
}

impl<D: Driver, T: ISender + IReceiver + Clone, R: Role> Clone for BridgeTmpl<D, T, R> {
    fn clone(&self) -> Self {
        Self {
            driver: self.driver.clone(),
            sq: self.sq.clone(),
            _marker: PhantomData,
        }
    }
}

impl<
    D: Driver,
    T: ISender<Item = <DUring<D> as UringSpec>::SQE>
        + IReceiver<Item = <DUring<D> as UringSpec>::CQE>
        + Clone,
> BridgeTmpl<D, T, Submit>
{
    /// submits a request in pending blocking.
    ///
    /// If the channel is closed, it will return an error.
    pub async fn submit(&self, data: D::SQE) -> Result<D::Op, <T as ISender>::Error> {
        let (id, op) = self.driver.register();
        let req = cell::IdCell::new(id, data);
        self.sq.send(req).await?;

        Ok(op)
    }

    /// submits a request in non-blocking.
    ///
    /// If the channel is closed or full, it will return an error immediately.
    pub fn try_submit(&self, data: D::SQE) -> Result<D::Op, <T as ISender>::TryError> {
        let (id, op) = self.driver.register();
        let req = cell::IdCell::new(id, data);
        self.sq.try_send(req)?;

        Ok(op)
    }
}

impl<
    D: Driver,
    T: ISender<Item = <DUring<D> as UringSpec>::SQE>
        + IReceiver<Item = <DUring<D> as UringSpec>::CQE>
        + Clone,
> BridgeTmpl<D, T, Receive>
{
    /// Receives completed msgs by blocking in pending.
    ///
    /// If the channel is empty or closed, it will block in pending.
    pub async fn complete(&self) {
        while let Ok(data) = self.sq.recv().await {
            let (id, payload) = data.into_inner();
            self.driver.complete(id, payload);
        }
    }

    /// Receives completed msgs in non-blocking.
    ///
    /// If the channel is empty or closed, it will return immediately.
    pub fn try_complete(&self) {
        while let Ok(data) = self.sq.try_recv() {
            let (id, payload) = data.into_inner();
            self.driver.complete(id, payload);
        }
    }
}

macro_rules! build_bridge {
    ($sq:expr) => {
        build_bridge!(@impl $sq, D::default())
    };

    ($sq:expr, $driver_cfg:expr) => {
        build_bridge!(@impl $sq, D::new($driver_cfg))
    };

    // The leading `@` is a convention for internal rules.
    (@impl $sq:expr, $driver_init:expr) => {{
        let d = $driver_init;
        (
            BridgeTmpl {
                driver: d.clone(),
                sq: $sq.clone(),
                _marker: PhantomData,
            },
            BridgeTmpl {
                driver: d,
                sq: $sq,
                _marker: PhantomData,
            },
        )
    }};
}

pub mod asynch {
    use core::marker::PhantomData;

    use crate::driver::{BridgeTmpl, Receive, Submit};
    use crate::{
        driver::{DUring, Driver},
        uring::asynch::{
            Completer as UCompleter, Submitter as USubmitter, channel, default_channel,
        },
    };

    pub type Submitter<D> = USubmitter<DUring<D>>;
    pub type Completer<D> = UCompleter<DUring<D>>;

    pub type Bridge<D, R> = BridgeTmpl<D, Submitter<D>, R>;
    pub type SubmitBridge<D> = Bridge<D, Submit>;
    pub type ReceiveBridge<D> = Bridge<D, Receive>;

    type UringBridge<D> = (SubmitBridge<D>, ReceiveBridge<D>, Completer<D>);

    pub fn new<D: Driver>(uring_cap: usize, driver_cfg: D::Config) -> UringBridge<D> {
        let (sq, cq) = channel::<DUring<D>>(uring_cap);
        let (sb, cb) = build_bridge!(sq, driver_cfg);
        (sb, cb, cq)
    }

    pub fn default<D: Driver>() -> UringBridge<D> {
        let (sq, cq) = default_channel::<DUring<D>>();
        let (sb, cb) = build_bridge!(sq);
        (sb, cb, cq)
    }
}

pub mod bare {
    use core::marker::PhantomData;

    use evering_shm::shm_alloc::ShmAllocator;

    use crate::driver::{BridgeTmpl, DUring, Driver, Receive, Submit};
    use crate::uring::UringSpec;
    use crate::uring::bare::{
        BoxQueue, Boxed, Completer as UringCompleter, Submitter as UringSubbmiter,
        box_channel, channel,
    };

    pub type Submitter<D, P, const N: usize> = UringSubbmiter<DUring<D>, P, N>;
    pub type Completer<D, P, const N: usize> = UringCompleter<DUring<D>, P, N>;

    pub type Bridge<D, P, const N: usize, R> = BridgeTmpl<D, Submitter<D, P, N>, R>;
    pub type SubmitBridge<D, P, const N: usize> = Bridge<D, P, N, Submit>;
    pub type ReceiveBridge<D, P, const N: usize> = Bridge<D, P, N, Receive>;

    pub type UringBridge<D, P, const N: usize> = (
        SubmitBridge<D, P, N>,
        ReceiveBridge<D, P, N>,
        Completer<D, P, N>,
    );
    pub type OwnUringBridge<D, const N: usize> = UringBridge<D, (), N>;
    pub type BoxUringBridge<D, A, const N: usize> = UringBridge<D, Boxed<A>, N>;

    pub fn own_new<D: Driver, const N: usize>(driver_cfg: D::Config) -> OwnUringBridge<D, N> {
        let (sq, cq) = channel::<DUring<D>, N>();
        let (sb, cb) = build_bridge!(sq, driver_cfg);
        (sb, cb, cq)
    }

    pub fn own_default<D: Driver, const N: usize>() -> OwnUringBridge<D, N> {
        let (sq, cq) = channel::<DUring<D>, N>();
        let (sb, cb) = build_bridge!(sq);
        (sb, cb, cq)
    }

    pub fn box_default<D: Driver, A: ShmAllocator, const N: usize>(
        q: (
            BoxQueue<<DUring<D> as UringSpec>::SQE, A, N>,
            BoxQueue<<DUring<D> as UringSpec>::CQE, A, N>,
        ),
    ) -> BoxUringBridge<D, A, N> {
        let (sq, cq) = box_channel::<DUring<D>, A, N>(q.0, q.1);
        let (sb, cb) = build_bridge!(sq);
        (sb, cb, cq)
    }

    pub fn box_client<D: Driver, A: ShmAllocator, const N: usize>(
        q: (
            BoxQueue<<DUring<D> as UringSpec>::SQE, A, N>,
            BoxQueue<<DUring<D> as UringSpec>::CQE, A, N>,
        ),
    ) -> (SubmitBridge<D, Boxed<A>, N>, ReceiveBridge<D, Boxed<A>, N>) {
        let (sq, _) = box_channel::<DUring<D>, A, N>(q.0, q.1);
        let (sb, cb) = build_bridge!(sq);
        (sb, cb)
    }

    pub fn box_server<D: Driver, A: ShmAllocator, const N: usize>(
        q: (
            BoxQueue<<DUring<D> as UringSpec>::SQE, A, N>,
            BoxQueue<<DUring<D> as UringSpec>::CQE, A, N>,
        ),
    ) -> Completer<D, Boxed<A>, N> {
        let (_, cq) = box_channel::<DUring<D>, A, N>(q.0, q.1);
        cq
    }
}
