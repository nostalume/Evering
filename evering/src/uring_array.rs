use alloc::sync::Arc;
use core::{iter, marker::PhantomData, ops::Deref};

use crossbeam_queue::ArrayQueue;

trait UringSpec {
    type SQE;
    type CQE;
}

#[derive(Debug)]
struct RawUring<S: UringSpec> {
    sq: ArrayQueue<S::SQE>,
    rq: ArrayQueue<S::CQE>,
}

impl<S: UringSpec> Into<Uring<S, CQ>> for RawUring<S> {
    fn into(self) -> Uring<S, CQ> {
        Uring {
            raw: Arc::new(self),
            _marker: PhantomData,
        }
    }
}

impl<S: UringSpec> Into<Uring<S, SQ>> for RawUring<S> {
    fn into(self) -> Uring<S, SQ> {
        Uring {
            raw: Arc::new(self),
            _marker: PhantomData,
        }
    }
}

impl<S: UringSpec> RawUring<S> {
    const CAP: usize = 1 << 5;
    fn raw(cap: usize) -> RawUring<S> {
        let sq = ArrayQueue::new(cap);
        let rq = ArrayQueue::new(cap);

        let raw = RawUring { sq, rq };
        raw
    }
}

impl<S: UringSpec> Default for RawUring<S> {
    fn default() -> Self {
        RawUring::raw(Self::CAP)
    }
}

impl<S: UringSpec, R: Role> Deref for Uring<S, R> {
    type Target = RawUring<S>;
    fn deref(&self) -> &Self::Target {
        &self.raw
    }
}

impl<S: UringSpec, R: Role> Clone for Uring<S, R> {
    fn clone(&self) -> Self {
        Uring {
            raw: self.raw.clone(),
            _marker: PhantomData,
        }
    }
}

trait Role {}
struct SQ;
impl Role for SQ {}
struct CQ;
impl Role for CQ {}

struct Uring<S: UringSpec, R: Role> {
    raw: Arc<RawUring<S>>,
    _marker: PhantomData<R>,
}

pub type SendRing<S> = Uring<S, SQ>;
pub type CompleteRing<S> = Uring<S, CQ>;

impl<S: UringSpec> Uring<S, SQ> {
    pub fn send(&self, sqe: S::SQE) -> Result<(), S::SQE> {
        self.sq.push(sqe)
    }

    pub fn send_bulk(&mut self, val: impl Iterator<Item = S::SQE>) -> usize {
        let mut count = 0;
        for sqe in val {
            if let Err(_) = self.send(sqe) {
                break;
            }
            count += 1;
        }
        count
    }

    pub fn recv(&self) -> Option<S::CQE> {
        self.rq.pop()
    }

    pub fn recv_bulk(&mut self) -> impl Iterator<Item = S::CQE> {
        iter::from_fn(|| self.rq.pop())
    }

    fn ref_cq(&self) -> Uring<S, CQ> {
        Uring {
            raw: self.raw.clone(),
            _marker: PhantomData,
        }
    }
}

impl<S: UringSpec> Uring<S, CQ> {
    pub fn send(&self, cqe: S::CQE) {
        self.rq.push(cqe);
    }
    pub fn recv(&self) -> Option<S::SQE> {
        self.sq.pop()
    }
    fn ref_sq(&self) -> Uring<S, SQ> {
        Uring {
            raw: self.raw.clone(),
            _marker: PhantomData,
        }
    }
}

fn new<S: UringSpec>(cap: usize) -> (SendRing<S>, CompleteRing<S>) {
    let sq: SendRing<S> = RawUring::raw(cap).into();
    let cq: CompleteRing<S> = sq.ref_cq();
    (sq, cq)
}

fn default<S: UringSpec>() -> (SendRing<S>, CompleteRing<S>) {
    let sq: SendRing<S> = RawUring::default().into();
    let cq: CompleteRing<S> = sq.ref_cq();
    (sq, cq)
}
