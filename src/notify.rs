use core::future::Future;

use crate::{
    AdoptError, Block, PoolRef, ReceiveError, Received, Rx, Transfer, TrySendError, Tx,
    channel::TransferReserved, layout::Repr, token::Shape,
};

const LOCAL_RETRY_LIMIT: usize = 32;

/// Sends an advisory notification after shared state has committed.
pub trait Notify {
    type Error;

    fn notify(&self) -> Result<(), Self::Error>;
}

/// Observes and consumes sticky advisory readiness in one operation.
pub trait Wait {
    type Error;

    fn wait(&self) -> impl Future<Output = Result<(), Self::Error>> + '_;
}

/// A committed shared operation and the health of its advisory notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Committed<T, E> {
    pub value: T,
    pub notified: Result<(), E>,
}

/// An asynchronous operation that did not commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgressError<E> {
    Busy,
    Closed,
    Wait(E),
}

/// Borrowed process-local progress capabilities for concrete Channel endpoints.
pub struct Signals<'a, N: ?Sized, W: ?Sized> {
    notify: &'a N,
    wait: &'a W,
}

fn committed<N: Notify + ?Sized, T>(notify: &N, value: T) -> Committed<T, N::Error> {
    Committed {
        value,
        notified: notify.notify(),
    }
}

impl<'a, N: ?Sized, W: ?Sized> Signals<'a, N, W> {
    pub const fn new(notify: &'a N, wait: &'a W) -> Self {
        Self { notify, wait }
    }
}

#[must_use = "dropping an unused permit cancels its Queue reservation"]
pub struct Permit<'n, 'q, H: Repr, N: Notify + ?Sized> {
    reserved: Option<TransferReserved<'q, H>>,
    notify: &'n N,
}

impl<N: Notify + ?Sized, W: Wait + ?Sized> Signals<'_, N, W> {
    #[expect(
        clippy::result_large_err,
        reason = "pre-commit failure returns the exact linear Transfer"
    )]
    pub fn try_send<'p, H: Repr>(
        &self,
        tx: &Tx<H>,
        value: Transfer<'p, H>,
    ) -> Result<Committed<(), N::Error>, TrySendError<Transfer<'p, H>>> {
        tx.try_send(value).map(|()| committed(self.notify, ()))
    }

    pub async fn reserve<'q, H: Repr>(
        &self,
        tx: &'q Tx<H>,
    ) -> Result<Permit<'_, 'q, H, N>, ProgressError<W::Error>> {
        let mut retries = 0;
        loop {
            match tx.reserve() {
                Ok(reserved) => {
                    return Ok(Permit {
                        reserved: Some(reserved),
                        notify: self.notify,
                    });
                }
                Err(crate::SendReserveError::Closed) => return Err(ProgressError::Closed),
                Err(crate::SendReserveError::Busy) if retries == LOCAL_RETRY_LIMIT - 1 => {
                    return Err(ProgressError::Busy);
                }
                Err(crate::SendReserveError::Busy) => {
                    retries += 1;
                    core::hint::spin_loop();
                }
                Err(crate::SendReserveError::Full) => {
                    retries = 0;
                    self.wait.wait().await.map_err(ProgressError::Wait)?;
                }
            }
        }
    }

    pub async fn claim<'q, H: Repr>(
        &self,
        rx: &'q Rx<H>,
    ) -> Result<Received<'q, H>, ProgressError<W::Error>> {
        let mut retries = 0;
        loop {
            match rx.claim() {
                Ok(received) => return Ok(received),
                Err(ReceiveError::Closed) => return Err(ProgressError::Closed),
                Err(ReceiveError::Busy) if retries == LOCAL_RETRY_LIMIT - 1 => {
                    return Err(ProgressError::Busy);
                }
                Err(ReceiveError::Busy) => {
                    retries += 1;
                    core::hint::spin_loop();
                }
                Err(ReceiveError::Empty) => {
                    retries = 0;
                    self.wait.wait().await.map_err(ProgressError::Wait)?;
                }
            }
        }
    }

    #[expect(
        clippy::type_complexity,
        reason = "the result exposes both committed notification health and retained claim authority"
    )]
    pub fn adopt<'q, 'p, H, T>(
        &self,
        received: Received<'q, H>,
        pool: PoolRef<'p>,
    ) -> Result<Committed<(H, Block<'p, T>), N::Error>, AdoptError<'q, H>>
    where
        H: Repr,
        T: Repr + Shape + ?Sized,
    {
        received
            .adopt(pool)
            .map(|value| committed(self.notify, value))
    }

    pub fn discard<'q, H: Repr>(
        &self,
        received: Received<'q, H>,
        pool: PoolRef<'_>,
    ) -> Result<Committed<H, N::Error>, AdoptError<'q, H>> {
        received
            .discard(pool)
            .map(|value| committed(self.notify, value))
    }

    pub fn close_tx<H: Repr>(&self, tx: &Tx<H>) -> Committed<(), N::Error> {
        tx.close();
        committed(self.notify, ())
    }

    pub fn close_rx<H: Repr>(&self, rx: &Rx<H>) -> Committed<(), N::Error> {
        rx.close();
        committed(self.notify, ())
    }
}

impl<H: Repr, N: Notify + ?Sized> Permit<'_, '_, H, N> {
    fn rollback(&mut self) {
        drop(self.reserved.take());
    }

    pub fn send<'p>(mut self, value: Transfer<'p, H>) -> Committed<(), N::Error> {
        self.reserved.take().unwrap().stage(value).publish();
        committed(self.notify, ())
    }

    pub fn cancel(mut self) -> Committed<(), N::Error> {
        self.rollback();
        committed(self.notify, ())
    }
}

impl<H: Repr, N: Notify + ?Sized> Drop for Permit<'_, '_, H, N> {
    fn drop(&mut self) {
        if self.reserved.is_some() {
            self.rollback();
            let _ = self.notify.notify();
        }
    }
}
