use crossbeam_channel::{Receiver, RecvError, SendError, Sender, TryRecvError, TrySendError};

use crate::{
    seal::Sealed,
    uring::{IReceiver, ISender, UringSpec},
};

pub type Uring<S> = (Submitter<S>, Completer<S>);

#[derive(Debug)]
pub struct Channel<T, U> {
    s: Sender<T>,
    r: Receiver<U>,
}

pub type Submitter<S: UringSpec> = Channel<S::SQE, S::CQE>;
pub type Completer<S: UringSpec> = Channel<S::CQE, S::SQE>;

impl<T, U> Clone for Channel<T, U> {
    fn clone(&self) -> Self {
        Self {
            s: self.s.clone(),
            r: self.r.clone(),
        }
    }
}

impl<T, U> Sealed for Channel<T, U> {}

impl<T, U> ISender for Channel<T, U> {
    type Item = T;
    type Error = SendError<T>;
    type TryError = TrySendError<T>;

    fn try_send(&self, item: Self::Item) -> Result<(), Self::TryError> {
        self.s.try_send(item)
    }

    async fn send(&self, item: Self::Item) -> Result<(), Self::Error> {
        self.s.send(item)
    }
}

impl<T, U> IReceiver for Channel<T, U> {
    type Item = U;
    type Error = RecvError;
    type TryError = TryRecvError;

    fn try_recv(&self) -> Result<Self::Item, Self::TryError> {
        self.r.try_recv()
    }

    async fn recv(&self) -> Result<Self::Item, Self::Error> {
        self.r.recv()
    }
}

pub fn channel<S: UringSpec>(cap: usize) -> Uring<S> {
    let (sqs, cqr) = crossbeam_channel::bounded(cap);
    let (cqs, sqr) = crossbeam_channel::bounded(cap);
    (Channel { s: sqs, r: sqr }, Channel { s: cqs, r: cqr })
}

pub fn default_channel<S: UringSpec>() -> Uring<S> {
    channel::<S>(crate::uring::DEFAULT_CAP)
}
