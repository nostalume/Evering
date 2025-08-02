use core::marker::PhantomData;

use async_channel::{SendError, TrySendError};

use crate::uring::UringSpec;
use crate::uring::asynch::{
    Completer as UringCompleter, Submitter as UringSubbmiter, channel, default_channel,
};

pub use crate::uring::asynch::{WithSink, WithStream};

pub mod locked;
mod op_cache;
mod unlocked;

pub trait Driver: UringSpec + Clone + Default {
    type Id;
    type Config: Default;
    type Op: Future<Output = Self::CQE>;
    fn register(&self) -> (Self::Id, Self::Op);
    fn complete(&self, id: Self::Id, payload: <Self::Op as Future>::Output);
    fn new(cfg: Self::Config) -> Self;
}

pub type Submitter<D> = UringSubbmiter<DriverUring<D>>;
pub type Completer<D> = UringCompleter<DriverUring<D>>;

pub struct DriverUring<D: Driver> {
    _marker: PhantomData<D>,
}

impl<D: Driver> UringSpec for DriverUring<D> {
    type SQE = (D::Id, D::SQE);
    type CQE = (D::Id, D::CQE);
}
/// The dispatched methods to handle submitted requests.
///
/// General case:
///     - kernel
///     - server
///     - ...
pub trait SQEHandle<D: Driver> {
    /// The output type for all handle methods. Defaults to `()`.
    type Output = ();

    // --- Blocking Handlers ---

    /// Dispatches and handles a submitted request in non-blocking.
    ///
    /// This method is called by the completer to process a submitted request synchronously.
    ///
    /// Users **must** implement this for their specific driver.
    fn try_handle(cq: Completer<D>) -> Self::Output {
        unimplemented!()
    }

    /// Dispatches and handles a submitted request in non-blocking,
    /// taking a reference to the completer.
    ///
    /// This method allows synchronous processing of a submitted request
    /// without consuming the completer.
    /// Users **must** implement this for their specific driver.
    fn try_handle_ref(cq: &Completer<D>) -> Self::Output {
        unimplemented!()
    }

    // --- Non-Blocking (Async) Handlers ---

    /// Dispatches and handles a submitted request in blocking pending.
    ///
    /// This method is called by the completer to process a submitted request asynchronously.
    /// Users **must** implement this for their specific driver.
    fn handle(cq: Completer<D>) -> impl Future<Output = Self::Output> {
        async { unimplemented!() }
    }

    /// Dispatches and handles a submitted request in blocking pending,
    /// taking a reference to the completer.
    ///
    /// This method allows asynchronous processing of a submitted request
    /// without consuming the completer.
    /// Users **must** implement this for their specific driver.
    fn handle_ref(cq: &Completer<D>) -> impl Future<Output = Self::Output> {
        async { unimplemented!() }
    }
}

pub struct Bridge<D: Driver, R: Role> {
    driver: D,
    sq: Submitter<D>,
    _marker: PhantomData<R>,
}

impl<D: Driver, R: Role> Clone for Bridge<D, R> {
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

pub type SubmitBridge<D> = Bridge<D, Submit>;
pub type CompleteBridge<D> = Bridge<D, Receive>;

type UringBridge<D> = (SubmitBridge<D>, CompleteBridge<D>, Completer<D>);

const URING_CAP: usize = 1 << 5;
pub fn new_with_cap<D: Driver>(
    uring_cap: usize,
    driver_cfg: D::Config,
) -> UringBridge< D> {
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

pub fn new< D: Driver>() -> UringBridge<D> {
    let (cq, sq) = default_channel();
    let d = D::new(D::Config::default());
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
        self.sq.sender().send((id, data)).await?;

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
        self.sq.sender().try_send((id, data))?;

        Ok(op)
    }
}

impl<D: Driver> Bridge<D, Receive> {
    /// Receives completed msgs by blocking in pending.
    ///
    /// If the channel is empty or closed, it will block in pending.
    pub async fn complete(&self) {
        while let Ok((id, payload)) = self.sq.receiver().recv().await {
            self.driver.complete(id, payload);
        }
    }

    /// Receives completed msgs in non-blocking.
    ///
    /// If the channel is empty or closed, it will return immediately.
    pub fn try_complete(&self) {
        while let Ok((id, payload)) = self.sq.receiver().try_recv() {
            self.driver.complete(id, payload);
        }
    }
}
