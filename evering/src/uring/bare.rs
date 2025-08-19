use core::{marker::PhantomData, ops::Deref, task::Poll};

use alloc::sync::Arc;
use evering_shm::{shm_alloc::ShmAllocator, shm_box::ShmBox};
use lfqueue::ConstBoundedQueue as CBQueue;

use crate::{
    seal::Sealed,
    uring::{Closable, IReceiver, ISender, UringSpec},
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

impl Sealed for &() {}
impl<'a, T, U, const N: usize> QPair<T, U, N> for &'a ()
where
    T: 'a,
    U: 'a,
{
    type Sender = RefQueue<'a, T, N>;
    type Receiver = RefQueue<'a, U, N>;
}

pub struct Boxed<A: ShmAllocator>(PhantomData<A>);
impl<A: ShmAllocator> Sealed for Boxed<A> {}
impl<T, U, A: ShmAllocator, const N: usize> QPair<T, U, N> for Boxed<A> {
    type Sender = ABoxQueue<T, A, N>;
    type Receiver = ABoxQueue<U, A, N>;
}

impl<A: ShmAllocator> Boxed<A> {
    pub fn new<T, const N: usize>(alloc: A) -> BoxQueue<T, A, N> {
        ShmBox::new_in(CBQueue::new_const(), alloc)
    }
}

pub trait Ref {
    type Target;
    fn as_ref(&self) -> &Self::Target;
}

impl<'a, T, const N: usize> Ref for RefQueue<'a, T, N> {
    type Target = CBQueue<T, N>;

    fn as_ref(&self) -> &Self::Target {
        self.deref()
    }
}

impl<T, const N: usize> Ref for ArcQueue<T, N> {
    type Target = CBQueue<T, N>;

    fn as_ref(&self) -> &Self::Target {
        self
    }
}

impl<T, A: ShmAllocator, const N: usize> Ref for ABoxQueue<T, A, N> {
    type Target = CBQueue<T, N>;

    fn as_ref(&self) -> &Self::Target {
        self.deref().as_ref()
    }
}

pub type ABoxQueue<T, A, const N: usize> = Arc<BoxQueue<T, A, N>>;
pub type BoxQueue<T, A, const N: usize> = ShmBox<CBQueue<T, N>, A>;

pub type RefQueue<'a, T, const N: usize> = Arc<BorrowQueue<'a, T, N>>;
pub type BorrowQueue<'a, T, const N: usize> = &'a CBQueue<T, N>;

pub type ArcQueue<T, const N: usize> = Arc<CBQueue<T, N>>;

// ---

pub type ABoxUring<S, A, const N: usize> = (ABoxSubmitter<S, A, N>, ABoxCompleter<S, A, N>);
pub type ABoxSubmitter<S, A, const N: usize> = Submitter<S, Boxed<A>, N>;
pub type ABoxCompleter<S, A, const N: usize> = Completer<S, Boxed<A>, N>;

pub type OwnUring<S, const N: usize> = (OwnSubmitter<S, N>, OwnCompleter<S, N>);
pub type OwnSubmitter<S, const N: usize> = Submitter<S, (), N>;
pub type OwnCompleter<S, const N: usize> = Completer<S, (), N>;

pub type RefUring<'a, S, const N: usize> = (RefSubmitter<'a, S, N>, RefCompleter<'a, S, N>);
pub type RefSubmitter<'a, S, const N: usize> = Submitter<S, &'a (), N>;
pub type RefCompleter<'a, S, const N: usize> = Completer<S, &'a (), N>;

pub type Submitter<S: UringSpec, P, const N: usize> = Channel<S::SQE, S::CQE, P, N>;
pub type Completer<S: UringSpec, P, const N: usize> = Channel<S::CQE, S::SQE, P, N>;

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

pub fn channel<S: UringSpec, const N: usize>() -> OwnUring<S, N> {
    let q1 = CBQueue::<S::SQE, N>::new_const();
    let q2 = CBQueue::<S::CQE, N>::new_const();
    build_qpair!(q1, q2)
}

pub fn default_channel<S: UringSpec>() -> OwnUring<S, { crate::uring::DEFAULT_CAP }> {
    channel::<S, { crate::uring::DEFAULT_CAP }>()
}

pub fn entrap_channel<'a, S: UringSpec, const N: usize>(
    q1: BorrowQueue<'a, S::SQE, N>,
    q2: BorrowQueue<'a, S::CQE, N>,
) -> RefUring<'a, S, N> {
    build_qpair!(q1, q2)
}

pub fn box_channel<S: UringSpec, A: ShmAllocator, const N: usize>(
    q1: BoxQueue<S::SQE, A, N>,
    q2: BoxQueue<S::CQE, A, N>,
) -> ABoxUring<S, A, N> {
    build_qpair!(q1, q2)
}
