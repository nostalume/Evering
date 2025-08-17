use core::{marker::PhantomData, ops::Deref};

use alloc::sync::Arc;
use evering_shm::{shm_alloc::ShmAllocator, shm_box::ShmBox};
use lfqueue::ConstBoundedQueue as CBQueue;

use crate::{seal::Sealed, uring::UringSpec};

pub trait QPair<S: UringSpec, const N: usize>: Sealed {
    type SubQueue: Ref<T = CBQueue<S::SQE, N>> + Clone;
    type CompQueue: Ref<T = CBQueue<S::CQE, N>> + Clone;
}

impl Sealed for () {}
impl<S: UringSpec, const N: usize> QPair<S, N> for () {
    type SubQueue = ArcQueue<S::SQE, N>;
    type CompQueue = ArcQueue<S::CQE, N>;
}

impl Sealed for &() {}
impl<'a, S: UringSpec, const N: usize> QPair<S, N> for &'a ()
where
    S::SQE: 'a,
    S::CQE: 'a,
{
    type SubQueue = RefQueue<'a, S::SQE, N>;
    type CompQueue = RefQueue<'a, S::CQE, N>;
}

pub struct Boxed<A: ShmAllocator>(PhantomData<A>);
impl<A: ShmAllocator> Sealed for Boxed<A> {}
impl<S: UringSpec, A: ShmAllocator, const N: usize> QPair<S, N> for Boxed<A> {
    type SubQueue = ABoxQueue<S::SQE, A, N>;
    type CompQueue = ABoxQueue<S::CQE, A, N>;
}

impl<A: ShmAllocator> Boxed<A> {
    pub fn new<T, const N: usize>(alloc: &A) -> BoxQueue<T, &A, N> {
        ShmBox::new_in(CBQueue::new_const(), alloc)
    }
}

pub trait Ref {
    type T;
    fn as_ref(&self) -> &Self::T;
}

impl<'a, T, const N: usize> Ref for RefQueue<'a, T, N> {
    type T = CBQueue<T, N>;

    fn as_ref(&self) -> &Self::T {
        self.deref()
    }
}

impl<T, const N: usize> Ref for ArcQueue<T, N> {
    type T = CBQueue<T, N>;

    fn as_ref(&self) -> &Self::T {
        self
    }
}

impl<T, A: ShmAllocator, const N: usize> Ref for ABoxQueue<T, A, N> {
    type T = CBQueue<T, N>;

    fn as_ref(&self) -> &Self::T {
        self.deref().as_ref()
    }
}

pub type ABoxUring<S, A, const N: usize> = (ABoxSubmitter<S, A, N>, ABoxCompleter<S, A, N>);
pub type ABoxSubmitter<S, A, const N: usize> = Submitter<S, Boxed<A>, N>;
pub type ABoxCompleter<S, A, const N: usize> = Completer<S, Boxed<A>, N>;

pub type OwnUring<S, const N: usize> = (OwnSubmitter<S, N>, OwnCompleter<S, N>);
pub type OwnSubmitter<S, const N: usize> = Submitter<S, (), N>;
pub type OwnCompleter<S, const N: usize> = Completer<S, (), N>;

pub type RefUring<'a, S, const N: usize> = (RefSubmitter<'a, S, N>, RefCompleter<'a, S, N>);
pub type RefSubmitter<'a, S, const N: usize> = Submitter<S, &'a (), N>;
pub type RefCompleter<'a, S, const N: usize> = Completer<S, &'a (), N>;

pub type ABoxQueue<T, A, const N: usize> = Arc<BoxQueue<T, A, N>>;
pub type BoxQueue<T, A, const N: usize> = ShmBox<CBQueue<T, N>, A>;

pub type RefQueue<'a, T, const N: usize> = Arc<BorrowQueue<'a, T, N>>;
pub type BorrowQueue<'a, T, const N: usize> = &'a CBQueue<T, N>;

pub type ArcQueue<T, const N: usize> = Arc<CBQueue<T, N>>;

pub type Submitter<S, P, const N: usize> = Channel<S, P, N, Submit>;
pub type Completer<S, P, const N: usize> = Channel<S, P, N, Complete>;

pub trait Role {}
pub struct Submit;
impl Role for Submit {}
pub struct Complete;
impl Role for Complete {}

pub struct Channel<S: UringSpec, P: QPair<S, N>, const N: usize, R: Role> {
    s: P::SubQueue,
    r: P::CompQueue,
    phantom: PhantomData<(S, R)>,
}

impl<S: UringSpec, P: QPair<S, N>, const N: usize, R: Role> Clone for Channel<S, P, N, R>
where
    P::SubQueue: Clone,
    P::CompQueue: Clone,
{
    fn clone(&self) -> Self {
        Self {
            s: self.s.clone(),
            r: self.r.clone(),
            phantom: self.phantom,
        }
    }
}

impl<S: UringSpec, P: QPair<S, N>, const N: usize> Submitter<S, P, N> {
    // block
    pub fn send(&self, data: S::SQE) {
        let mut d = Some(data);
        loop {
            let cur = d.take().unwrap();
            match self.s.as_ref().enqueue(cur) {
                Ok(_) => break,
                Err(cur) => {
                    d = Some(cur);
                    core::hint::spin_loop();
                }
            }
        }
    }

    pub fn try_send(&self, data: S::SQE) -> Result<(), S::SQE> {
        self.s.as_ref().enqueue(data)
    }

    pub fn recv(&self) -> S::CQE {
        loop {
            if let Some(d) = self.r.as_ref().dequeue() {
                return d;
            }
        }
    }

    pub fn try_recv(&self) -> Option<S::CQE> {
        self.r.as_ref().dequeue()
    }
}

impl<S: UringSpec, P: QPair<S, N>, const N: usize> Completer<S, P, N> {
    // block
    pub fn send(&self, data: S::CQE) {
        let mut d = Some(data);
        loop {
            let cur = d.take().unwrap();
            match self.r.as_ref().enqueue(cur) {
                Ok(_) => break,
                Err(cur) => {
                    d = Some(cur);
                    core::hint::spin_loop();
                }
            }
        }
    }

    pub fn try_send(&self, data: S::CQE) -> Result<(), S::CQE> {
        self.r.as_ref().enqueue(data)
    }

    pub fn recv(&self) -> S::SQE {
        loop {
            if let Some(d) = self.s.as_ref().dequeue() {
                return d;
            }
        }
    }

    pub fn try_recv(&self) -> Option<S::SQE> {
        self.s.as_ref().dequeue()
    }
}

macro_rules! build_qpair {
    ($q1:expr, $q2:expr, $submitter:ident, $completer:ident) => {{
        let s = Arc::new($q1);
        let r = Arc::new($q2);
        (
            $submitter {
                s: s.clone(),
                r: r.clone(),
                phantom: PhantomData,
            },
            $completer {
                s,
                r,
                phantom: PhantomData,
            },
        )
    }};
}

pub fn channel<S: UringSpec, const N: usize>() -> OwnUring<S, N> {
    let s = CBQueue::<S::SQE, N>::new_const();
    let r = CBQueue::<S::CQE, N>::new_const();
    build_qpair!(s, r, OwnSubmitter, OwnCompleter)
}

pub fn default_channel<S: UringSpec>() -> OwnUring<S, { crate::uring::DEFAULT_CAP }> {
    channel::<S, { crate::uring::DEFAULT_CAP }>()
}

pub fn entrap_channel<'a, S: UringSpec, const N: usize>(
    q1: BorrowQueue<'a, S::SQE, N>,
    q2: BorrowQueue<'a, S::CQE, N>,
) -> RefUring<'a, S, N> {
    build_qpair!(q1, q2, RefSubmitter, RefCompleter)
}

pub fn box_channel<S: UringSpec, A: ShmAllocator, const N: usize>(
    q1: BoxQueue<S::SQE, A, N>,
    q2: BoxQueue<S::CQE, A, N>,
) -> ABoxUring<S, A, N> {
    build_qpair!(q1, q2, ABoxSubmitter, ABoxCompleter)
}
