use core::{
    ops::Deref,
    pin,
    task::{self, Poll},
};

use crossbeam_channel::{Receiver, Sender, TryRecvError, TrySendError};
use futures::{Sink, Stream};

pub trait UringSpec {
    type SQE;
    type CQE;
}
pub trait WithSend {
    type T;
    fn sink_sender(&self) -> &SinkSender<Self::T>;
    fn sender(&self) -> &Sender<Self::T> {
        &self.sink_sender()
    }
    fn send_bulk(&self, vals: impl Iterator<Item = Self::T>) -> usize {
        let mut vals = vals;
        let mut count = 0;
        loop {
            let Some(val) = vals.next() else { break count };

            if let Err(_) = self.sender().send(val) {
                break count;
            }

            count += 1;
        }
    }
}

#[derive(Debug, Clone)]
pub struct SinkSender<T>(Sender<T>);

impl<T> From<Sender<T>> for SinkSender<T> {
    fn from(sender: Sender<T>) -> Self {
        Self(sender)
    }
}

impl<T> AsRef<Sender<T>> for SinkSender<T> {
    fn as_ref(&self) -> &Sender<T> {
        &self.0
    }
}

impl<T> Deref for SinkSender<T> {
    type Target = Sender<T>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> Sink<T> for SinkSender<T> {
    type Error = TrySendError<T>;

    fn poll_ready(
        self: pin::Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        if this.is_full() {
            return Poll::Pending;
        }
        Poll::Ready(Ok(()))
    }

    fn start_send(self: pin::Pin<&mut Self>, item: T) -> Result<(), Self::Error> {
        self.0.try_send(item)
    }

    fn poll_flush(
        self: pin::Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        let this = self.get_mut();
        if this.is_full() {
            return Poll::Pending;
        }
        Poll::Ready(Ok(()))
    }

    fn poll_close(
        self: pin::Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

pub trait WithRecv {
    type T;
    fn stream_receiver(&self) -> &StreamReceiver<Self::T>;
    fn receiver(&self) -> &Receiver<Self::T> {
        self.stream_receiver()
    }
    fn recv_bulk(&self) -> impl Iterator<Item = Self::T> {
        core::iter::from_fn(|| self.receiver().recv().ok())
    }
}

#[derive(Debug, Clone)]
pub struct StreamReceiver<T>(Receiver<T>);

impl<T> From<Receiver<T>> for StreamReceiver<T> {
    fn from(receiver: Receiver<T>) -> Self {
        Self(receiver)
    }
}

impl<T> AsRef<Receiver<T>> for StreamReceiver<T> {
    fn as_ref(&self) -> &Receiver<T> {
        &self.0
    }
}

impl<T> Deref for StreamReceiver<T> {
    type Target = Receiver<T>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T: Unpin> Stream for StreamReceiver<T> {
    type Item = T;

    fn poll_next(
        self: pin::Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.try_recv() {
            Ok(msg) => Poll::Ready(Some(msg)),
            Err(TryRecvError::Empty) => Poll::Pending,
            Err(TryRecvError::Disconnected) => Poll::Ready(None),
        }
    }
}

pub type Uring<S> = (Completer<S>, Submitter<S>);

#[derive(Debug, Clone)]
pub struct Submitter<S: UringSpec> {
    sqs: SinkSender<S::SQE>,
    sqr: StreamReceiver<S::CQE>,
}
#[derive(Debug, Clone)]
pub struct Completer<S: UringSpec> {
    cqs: SinkSender<S::CQE>,
    cqr: StreamReceiver<S::SQE>,
}

impl<S: UringSpec> WithSend for Submitter<S> {
    type T = S::SQE;
    fn sink_sender(&self) -> &SinkSender<Self::T> {
        &self.sqs
    }
}

impl<S: UringSpec> WithRecv for Submitter<S> {
    type T = S::CQE;
    fn stream_receiver(&self) -> &StreamReceiver<Self::T> {
        &self.sqr
    }
}

impl<S: UringSpec> WithSend for Completer<S> {
    type T = S::CQE;
    fn sink_sender(&self) -> &SinkSender<Self::T> {
        &self.cqs
    }
}

impl<S: UringSpec> WithRecv for Completer<S> {
    type T = S::SQE;
    fn stream_receiver(&self) -> &StreamReceiver<Self::T> {
        &self.cqr
    }
}

pub fn channel<S: UringSpec>(cap: usize) -> Uring<S> {
    let (cqs, sqr) = crossbeam_channel::bounded(cap);
    let (sqs, cqr) = crossbeam_channel::bounded(cap);
    (
        Completer {
            cqs: cqs.into(),
            cqr: cqr.into(),
        },
        Submitter {
            sqs: sqs.into(),
            sqr: sqr.into(),
        },
    )
}

pub fn default<S: UringSpec>() -> Uring<S> {
    let cap = 1 << 5;
    channel(cap)
}
