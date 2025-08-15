use alloc::alloc::{Allocator, Global};
use async_channel::{Receiver, Sender};

use crate::uring::{UringSpec, with_recv_alloc, with_send_alloc};

pub type Uring<S, A: Allocator = Global> = (Completer<S, A>, Submitter<S, A>);

#[derive(Debug)]
pub struct Submitter<S: UringSpec, A: Allocator = Global> {
    sqs: Sender<S::SQE, A>,
    sqr: Receiver<S::CQE, A>,
}
#[derive(Debug)]
pub struct Completer<S: UringSpec, A: Allocator = Global> {
    cqs: Sender<S::CQE, A>,
    cqr: Receiver<S::SQE, A>,
}

impl<S: UringSpec, A: Allocator> Clone for Submitter<S, A> {
    fn clone(&self) -> Self {
        Self {
            sqs: self.sqs.clone(),
            sqr: self.sqr.clone(),
        }
    }
}

impl<S: UringSpec, A: Allocator> Clone for Completer<S, A> {
    fn clone(&self) -> Self {
        Self {
            cqs: self.cqs.clone(),
            cqr: self.cqr.clone(),
        }
    }
}

with_send_alloc!(Submitter, sqs, Sender, SQE);
with_recv_alloc!(Submitter, sqr, Receiver, CQE);
with_send_alloc!(Completer, cqs, Sender, CQE);
with_recv_alloc!(Completer, cqr, Receiver, SQE);

pub fn channel<S: UringSpec>(cap: usize) -> Uring<S> {
    let (cqs, sqr) = async_channel::bounded(cap);
    let (sqs, cqr) = async_channel::bounded(cap);
    (Completer { cqs, cqr }, Submitter { sqs, sqr })
}

pub fn channel_in<S: UringSpec, A: Allocator>(cap: usize, alloc: &A) -> Uring<S, &A> {
    let (cqs, sqr) = async_channel::bounded_in(cap, alloc);
    let (sqs, cqr) = async_channel::bounded_in(cap, alloc);
    (Completer { cqs, cqr }, Submitter { sqs, sqr })
}

const DEFAULT_CAP: usize = 1 << 5;

pub fn default_channel<S: UringSpec>() -> Uring<S> {
    channel(DEFAULT_CAP)
}

pub fn default_channel_in<S: UringSpec, A: Allocator>(alloc: &A) -> Uring<S, &A> {
    channel_in(DEFAULT_CAP, alloc)
}
