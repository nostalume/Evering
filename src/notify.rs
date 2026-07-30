use core::future::Future;

use crate::channel::{Receiver, Sender, TryRecvError, TrySendError};

/// Sends an advisory notification after shared state has committed.
pub trait Notify {
    type Error;

    fn notify(&self) -> Result<(), Self::Error>;
}

impl<T: Notify + ?Sized> Notify for &T {
    type Error = T::Error;

    fn notify(&self) -> Result<(), Self::Error> {
        T::notify(self)
    }
}

/// Waits for and clears sticky advisory readiness.
pub trait Listen {
    type Error;
    type Ready<'a>: Future<Output = Result<(), Self::Error>>
    where
        Self: 'a;

    fn ready(&self) -> Self::Ready<'_>;
    fn clear(&self) -> Result<(), Self::Error>;
}

impl<T: Listen + ?Sized> Listen for &T {
    type Error = T::Error;
    type Ready<'a>
        = T::Ready<'a>
    where
        Self: 'a;

    fn ready(&self) -> Self::Ready<'_> {
        T::ready(self)
    }

    fn clear(&self) -> Result<(), Self::Error> {
        T::clear(self)
    }
}

/// A committed shared operation and the health of its advisory notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Done<T, E> {
    pub value: T,
    pub notified: Result<(), E>,
}

/// A send that did not commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendError<E> {
    Disconnected,
    Wait(E),
}

/// Caller-owned state for a send that may suspend before committing.
#[derive(Debug)]
#[must_use = "an uncommitted send still owns its value"]
pub struct Pending<T>(Option<T>);

impl<T> Pending<T> {
    pub const fn new(value: T) -> Self {
        Self(Some(value))
    }

    pub fn into_inner(self) -> Option<T> {
        self.0
    }
}

/// A receive that did not commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecvError<E> {
    Disconnected,
    Wait(E),
}

/// Adds advisory waiting to an unchanged nonblocking endpoint.
pub struct Async<T, N, L> {
    inner: T,
    notify: N,
    listen: L,
}

impl<T, N, L> Async<T, N, L> {
    pub const fn new(inner: T, notify: N, listen: L) -> Self {
        Self {
            inner,
            notify,
            listen,
        }
    }

    pub fn into_parts(self) -> (T, N, L) {
        (self.inner, self.notify, self.listen)
    }
}

impl<T, N, L> Async<T, N, L>
where
    T: crate::channel::QueueChannel,
    N: Notify,
{
    /// Closes the shared endpoint, then advises its peer to recheck the gate.
    pub fn close(&self) -> Done<(), N::Error> {
        self.inner.close();
        Done {
            value: (),
            notified: self.notify.notify(),
        }
    }

    pub fn is_closed(&self) -> bool {
        self.inner.is_close()
    }
}

impl<T, N, L, V> Async<T, N, L>
where
    T: Sender<Item = V, TryError = TrySendError<V>>,
    N: Notify,
    L: Listen,
{
    pub async fn send(
        &self,
        pending: &mut Pending<V>,
    ) -> Result<Done<(), N::Error>, SendError<L::Error>> {
        loop {
            let value = pending.0.take().expect("cannot resend a committed value");
            match self.inner.try_send(value) {
                Ok(()) => {
                    return Ok(Done {
                        value: (),
                        notified: self.notify.notify(),
                    });
                }
                Err(TrySendError::Disconnected(returned)) => {
                    pending.0 = Some(returned);
                    return Err(SendError::Disconnected);
                }
                Err(TrySendError::Full(returned)) => {
                    pending.0 = Some(returned);
                    if let Err(error) = self.listen.ready().await {
                        return Err(SendError::Wait(error));
                    }
                    if let Err(error) = self.listen.clear() {
                        return Err(SendError::Wait(error));
                    }
                }
            }
        }
    }
}

impl<T, N, L, V> Async<T, N, L>
where
    T: Receiver<Item = V, TryError = TryRecvError>,
    N: Notify,
    L: Listen,
{
    pub async fn recv(&self) -> Result<Done<V, N::Error>, RecvError<L::Error>> {
        loop {
            match self.inner.try_recv() {
                Ok(value) => {
                    return Ok(Done {
                        value,
                        notified: self.notify.notify(),
                    });
                }
                Err(TryRecvError::Disconnected) => return Err(RecvError::Disconnected),
                Err(TryRecvError::Empty) => {
                    self.listen.ready().await.map_err(RecvError::Wait)?;
                    self.listen.clear().map_err(RecvError::Wait)?;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Async, Done, Listen, Notify, Pending, RecvError, SendError};
    use crate::channel::{Receiver, Sender, TryRecvError, TrySendError};
    use core::{
        cell::Cell,
        future::{Ready, ready},
    };

    struct Bell(Result<(), u8>);

    impl Notify for Bell {
        type Error = u8;

        fn notify(&self) -> Result<(), Self::Error> {
            self.0
        }
    }

    struct Latch {
        ready: Cell<bool>,
        clears: Cell<usize>,
        error: Option<u8>,
    }

    impl Listen for Latch {
        type Error = u8;
        type Ready<'a> = Ready<Result<(), u8>>;

        fn ready(&self) -> Self::Ready<'_> {
            self.ready.set(true);
            ready(self.error.map_or(Ok(()), Err))
        }

        fn clear(&self) -> Result<(), Self::Error> {
            self.clears.set(self.clears.get() + 1);
            Ok(())
        }
    }

    struct Tx<'a> {
        latch: &'a Latch,
        tries: Cell<usize>,
    }

    impl Sender for Tx<'_> {
        type Item = u8;
        type TryError = TrySendError<u8>;

        fn try_send(&self, item: u8) -> Result<(), Self::TryError> {
            self.tries.set(self.tries.get() + 1);
            if self.latch.ready.get() {
                Ok(())
            } else {
                Err(TrySendError::Full(item))
            }
        }
    }

    struct Rx<'a> {
        latch: &'a Latch,
    }

    impl Receiver for Rx<'_> {
        type Item = u8;
        type TryError = TryRecvError;

        fn try_recv(&self) -> Result<u8, Self::TryError> {
            if self.latch.ready.get() {
                Ok(29)
            } else {
                Err(TryRecvError::Empty)
            }
        }
    }

    struct Never;

    impl Listen for Never {
        type Error = u8;
        type Ready<'a> = core::future::Pending<Result<(), u8>>;

        fn ready(&self) -> Self::Ready<'_> {
            core::future::pending()
        }

        fn clear(&self) -> Result<(), Self::Error> {
            unreachable!("permanently pending readiness cannot be cleared")
        }
    }

    struct Full<'a>(&'a Cell<usize>);

    impl Sender for Full<'_> {
        type Item = u8;
        type TryError = TrySendError<u8>;

        fn try_send(&self, item: u8) -> Result<(), Self::TryError> {
            self.0.set(self.0.get() + 1);
            Err(TrySendError::Full(item))
        }
    }

    struct Closed<'a>(&'a Latch);

    impl Sender for Closed<'_> {
        type Item = u8;
        type TryError = TrySendError<u8>;

        fn try_send(&self, item: u8) -> Result<(), Self::TryError> {
            if self.0.ready.get() {
                Err(TrySendError::Disconnected(item))
            } else {
                Err(TrySendError::Full(item))
            }
        }
    }

    #[tokio::test]
    async fn cancelled_send_keeps_caller_owned_value() {
        let tries = Cell::new(0);
        let mut value = Pending::new(73);
        let send = Async::new(Full(&tries), Bell(Ok(())), Never);

        assert!(
            tokio::time::timeout(core::time::Duration::from_millis(1), send.send(&mut value))
                .await
                .is_err()
        );
        assert_eq!(tries.get(), 1);
        assert_eq!(value.into_inner(), Some(73));
    }

    #[tokio::test]
    async fn close_while_waiting_keeps_caller_owned_value() {
        let latch = Latch {
            ready: Cell::new(false),
            clears: Cell::new(0),
            error: None,
        };
        let mut value = Pending::new(31);
        let result = Async::new(Closed(&latch), Bell(Ok(())), &latch)
            .send(&mut value)
            .await;

        assert_eq!(result, Err(SendError::Disconnected));
        assert_eq!(value.into_inner(), Some(31));
        assert_eq!(latch.clears.get(), 1);
    }

    #[tokio::test]
    async fn full_and_empty_wait_clear_and_retry_authoritative_state() {
        let tx_latch = Latch {
            ready: Cell::new(false),
            clears: Cell::new(0),
            error: None,
        };
        let tx = Tx {
            latch: &tx_latch,
            tries: Cell::new(0),
        };
        let mut value = Pending::new(17);
        let sent = Async::new(tx, Bell(Ok(())), &tx_latch)
            .send(&mut value)
            .await;
        assert_eq!(
            sent,
            Ok(Done {
                value: (),
                notified: Ok(())
            })
        );
        assert_eq!(value.into_inner(), None);
        assert_eq!(tx_latch.clears.get(), 1);

        let rx_latch = Latch {
            ready: Cell::new(false),
            clears: Cell::new(0),
            error: None,
        };
        let received = Async::new(Rx { latch: &rx_latch }, Bell(Ok(())), &rx_latch)
            .recv()
            .await;
        assert_eq!(
            received,
            Ok(Done {
                value: 29,
                notified: Ok(())
            })
        );
        assert_eq!(rx_latch.clears.get(), 1);
    }

    #[tokio::test]
    async fn wait_failure_preserves_uncommitted_send_ownership() {
        let latch = Latch {
            ready: Cell::new(false),
            clears: Cell::new(0),
            error: Some(7),
        };
        let mut value = Pending::new(41);
        let result = Async::new(
            Tx {
                latch: &latch,
                tries: Cell::new(0),
            },
            Bell(Ok(())),
            &latch,
        )
        .send(&mut value)
        .await;
        assert_eq!(result, Err(SendError::Wait(7)));
        assert_eq!(value.into_inner(), Some(41));
    }

    #[tokio::test]
    async fn post_commit_notify_failure_is_not_a_precommit_error() {
        let latch = Latch {
            ready: Cell::new(true),
            clears: Cell::new(0),
            error: None,
        };
        let mut value = Pending::new(53);
        let sent = Async::new(
            Tx {
                latch: &latch,
                tries: Cell::new(0),
            },
            Bell(Err(9)),
            &latch,
        )
        .send(&mut value)
        .await;
        assert_eq!(
            sent,
            Ok(Done {
                value: (),
                notified: Err(9)
            })
        );
        assert_eq!(value.into_inner(), None);

        let _ = RecvError::<u8>::Disconnected;
    }
}
