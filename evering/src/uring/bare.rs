use core::task::Poll;

use alloc::sync::Arc;
use lfqueue::ConstBoundedQueue as CBQueue;

use crate::{
    seal::Sealed,
    uring::{IReceiver, ISender, UringSpec},
};

pub trait QPair<T, U, const N: usize>: Sealed {
    type Sender: Ref<Target = CBQueue<T, N>> + Clone;
    type Receiver: Ref<Target = CBQueue<U, N>> + Clone;
}

impl Sealed for () {}
impl<T, U, const N: usize> QPair<T, U, N> for () {
    type Sender = ArcQueue<T, N>;
    type Receiver = ArcQueue<U, N>;
}

pub trait Ref {
    type Target;
    fn as_ref(&self) -> &Self::Target;
}

impl<T, const N: usize> Ref for ArcQueue<T, N> {
    type Target = CBQueue<T, N>;

    fn as_ref(&self) -> &Self::Target {
        self
    }
}

pub type ArcQueue<T, const N: usize> = Arc<CBQueue<T, N>>;
// ---
pub type Uring<S, const N: usize> = (Submitter<S, N>, Completer<S, N>);
pub type Submitter<S, const N: usize> = SubmitterIn<S, (), N>;
pub type Completer<S, const N: usize> = CompleterIn<S, (), N>;

pub type SubmitterIn<S: UringSpec, P, const N: usize> = Channel<S::SQE, S::CQE, P, N>;
pub type CompleterIn<S: UringSpec, P, const N: usize> = Channel<S::CQE, S::SQE, P, N>;

pub struct Channel<T, U, P: QPair<T, U, N>, const N: usize> {
    s: P::Sender,
    r: P::Receiver,
}

unsafe impl<T, U, P: QPair<T, U, N>, const N: usize> Send for Channel<T, U, P, N> {}
unsafe impl<T, U, P: QPair<T, U, N>, const N: usize> Sync for Channel<T, U, P, N> {}

impl<T, U, P: QPair<T, U, N>, const N: usize> Clone for Channel<T, U, P, N>
where
    P::Sender: Clone,
    P::Receiver: Clone,
{
    fn clone(&self) -> Self {
        Self {
            s: self.s.clone(),
            r: self.r.clone(),
        }
    }
}

impl<T, U, P: QPair<T, U, N>, const N: usize> Sealed for Channel<T, U, P, N> {}

impl<T, U, P: QPair<T, U, N>, const N: usize> ISender for Channel<T, U, P, N> {
    type Item = T;
    type Error = T;
    type TryError = T;

    fn try_send(&self, item: Self::Item) -> Result<(), Self::TryError> {
        self.s.as_ref().enqueue(item)
    }
    async fn send(&self, item: Self::Item) -> Result<(), Self::Error> {
        let mut item = Some(item);
        core::future::poll_fn(|cx| {
            loop {
                let cur = match item.take() {
                    Some(i) => i,
                    None => return Poll::Ready(Ok(())),
                };
                match self.s.as_ref().enqueue(cur) {
                    Ok(()) => return Poll::Ready(Ok(())),
                    Err(e) => {
                        item.replace(e);
                        core::hint::spin_loop();
                        cx.waker().wake_by_ref();
                        return Poll::Pending;
                    }
                }
            }
        })
        .await
    }
}

impl<T, U, P: QPair<T, U, N>, const N: usize> IReceiver for Channel<T, U, P, N> {
    type Item = U;
    type Error = ();
    type TryError = ();
    fn try_recv(&self) -> Result<Self::Item, Self::TryError> {
        self.r.as_ref().dequeue().ok_or(())
    }

    async fn recv(&self) -> Result<Self::Item, Self::Error> {
        core::future::poll_fn(|cx| {
            loop {
                match self.r.as_ref().dequeue() {
                    Some(item) => {
                        return Poll::Ready(Ok(item));
                    }
                    None => {
                        core::hint::spin_loop();
                        cx.waker().wake_by_ref();
                        return Poll::Pending;
                    }
                }
            }
        })
        .await
    }
}

macro_rules! build_qpair {
    ($q1:expr, $q2:expr) => {{
        let q1 = Arc::new($q1);
        let q2 = Arc::new($q2);
        (
            Channel {
                s: q1.clone(),
                r: q2.clone(),
            },
            Channel { s: q2, r: q1 },
        )
    }};
}

pub fn channel<S: UringSpec, const N: usize>() -> Uring<S, N> {
    let q1 = CBQueue::<S::SQE, N>::new_const();
    let q2 = CBQueue::<S::CQE, N>::new_const();
    build_qpair!(q1, q2)
}

pub fn default_channel<S: UringSpec>() -> Uring<S, { crate::uring::DEFAULT_CAP }> {
    channel::<S, { crate::uring::DEFAULT_CAP }>()
}
