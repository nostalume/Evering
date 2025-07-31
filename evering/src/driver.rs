use core::marker::PhantomData;

use async_channel::{SendError, TrySendError};
use lock_api::RawMutex;

use crate::driver::inner::{Driver, Op, OpId};
use crate::uring::UringSpec;
use crate::uring::asynch::{Completer as UringCompleter, Submitter as UringSubbmiter, channel};

pub use crate::uring::asynch::{WithSink, WithStream};
mod inner;

pub mod lock {
    pub type SpinMutex = spin::Mutex<()>;
}

pub trait DriverSpec {
    type Lock: RawMutex;
    // ...
}

pub type Submitter<S> = UringSubbmiter<DriverUring<S>>;
pub type Completer<S> = UringCompleter<DriverUring<S>>;

/// The dispatched methods to handle submitted requests.
///
/// General case:
///     - kernel
///     - server
///     - ...
pub trait SQEHandle<U:UringSpec>{
    /// The output type for all handle methods. Defaults to `()`.
    type HandleOutput = ();

    // --- Blocking Handlers ---

    /// Dispatches and handles a submitted request in non-blocking.
    ///
    /// This method is called by the completer to process a submitted request synchronously.
    ///
    /// Users **must** implement this for their specific driver.
    fn try_handle(cq: Completer<U>) -> Self::HandleOutput {
        unimplemented!()
    }

    /// Dispatches and handles a submitted request in non-blocking,
    /// taking a reference to the completer.
    ///
    /// This method allows synchronous processing of a submitted request
    /// without consuming the completer.
    /// Users **must** implement this for their specific driver.
    fn try_handle_ref(cq: &Completer<U>) -> Self::HandleOutput {
        unimplemented!()
    }

    // --- Non-Blocking (Async) Handlers ---

    /// Dispatches and handles a submitted request in blocking pending.
    ///
    /// This method is called by the completer to process a submitted request asynchronously.
    /// Users **must** implement this for their specific driver.
    fn handle(cq: Completer<U>) -> impl Future<Output = Self::HandleOutput> {
        async { unimplemented!() }
    }

    /// Dispatches and handles a submitted request in blocking pending,
    /// taking a reference to the completer.
    ///
    /// This method allows asynchronous processing of a submitted request
    /// without consuming the completer.
    /// Users **must** implement this for their specific driver.
    fn handle_ref(cq: &Completer<U>) -> impl Future<Output = Self::HandleOutput> {
        async { unimplemented!() }
    }
}

pub struct DriverUring<S: UringSpec> {
    _marker: PhantomData<S>,
}

impl<S: UringSpec> UringSpec for DriverUring<S> {
    type SQE = (OpId, S::SQE);
    type CQE = (OpId, S::CQE);
}

pub struct Bridge<S: UringSpec, D: DriverSpec, R: Role> {
    driver: Driver<S, D>,
    sq: Submitter<S>,
    _marker: PhantomData<R>,
}

impl<S: UringSpec, D: DriverSpec, R: Role> Clone for Bridge<S, D, R> {
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

pub type SubmitBridge<S, D> = Bridge<S, D, Submit>;
pub type CompleteBridge<S, D> = Bridge<S, D, Receive>;

type UringBridge<S, D> = (SubmitBridge<S, D>, CompleteBridge<S, D>, Completer<S>);

const URING_CAP: usize = 1 << 5;
const DRIVER_CAP: usize = 1 << 10;
pub fn new_with_cap<S: UringSpec, D: DriverSpec>(
    uring_cap: usize,
    driver_cap: usize,
) -> UringBridge<S, D> {
    let (cq, sq) = channel(uring_cap);
    let d = Driver::new_with_cap(driver_cap);
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

pub fn new<S: UringSpec, D: DriverSpec>() -> UringBridge<S, D> {
    new_with_cap(URING_CAP, DRIVER_CAP)
}

impl<S: UringSpec, D: DriverSpec> Bridge<S, D, Submit> {
    fn register(&self) -> (OpId, Op<S, D>) {
        let mut core = self.driver.inner.lock();
        let id = core.insert();
        let op = Op {
            id,
            driver: self.driver.clone(),
        };

        (id, op)
    }
    /// submits a request in pending blocking.
    ///
    /// If the channel is closed, it will return an error.
    pub async fn submit(
        &self,
        data: S::SQE,
    ) -> Result<Op<S, D>, SendError<<DriverUring<S> as UringSpec>::SQE>> {
        let (id, op) = self.register();
        self.sq.sender().send((id, data)).await?;

        Ok(op)
    }

    /// submits a request in non-blocking.
    ///
    /// If the channel is closed or full, it will return an error immediately.
    pub fn try_submit(
        &self,
        data: S::SQE,
    ) -> Result<Op<S, D>, TrySendError<<DriverUring<S> as UringSpec>::SQE>> {
        let (id, op) = self.register();
        self.sq.sender().try_send((id, data))?;

        Ok(op)
    }
}

impl<S: UringSpec, D: DriverSpec> Bridge<S, D, Receive> {
    fn deregister(&self, id: OpId, payload: S::CQE) {
        let mut core = self.driver.inner.lock();
        // if the op is dropped, `op_sign` won't exist.
        if let Some(op_sign) = core.get_mut(id) {
            op_sign.complete(payload);
        }
    }
    /// Receives completed msgs by blocking in pending.
    ///
    /// If the channel is empty or closed, it will block in pending.
    pub async fn complete(&self) {
        while let Ok((id, payload)) = self.sq.receiver().recv().await {
            self.deregister(id, payload);
        }
    }

    /// Receives completed msgs in non-blocking.
    ///
    /// If the channel is empty or closed, it will return immediately.
    pub fn try_complete(&self) {
        while let Ok((id, payload)) = self.sq.receiver().try_recv() {
            self.deregister(id, payload);
        }
    }
}
