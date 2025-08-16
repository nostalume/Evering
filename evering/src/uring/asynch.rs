use async_channel::{Receiver, Sender};

use crate::uring::{UringSpec, with_recv, with_send};

pub type Uring<S> = (Completer<S>, Submitter<S>);

#[derive(Debug)]
pub struct Submitter<S: UringSpec> {
    sqs: Sender<S::SQE>,
    sqr: Receiver<S::CQE>,
}
#[derive(Debug)]
pub struct Completer<S: UringSpec> {
    cqs: Sender<S::CQE>,
    cqr: Receiver<S::SQE>,
}

impl<S: UringSpec> Clone for Submitter<S> {
    fn clone(&self) -> Self {
        Self {
            sqs: self.sqs.clone(),
            sqr: self.sqr.clone(),
        }
    }
}

impl<S: UringSpec> Clone for Completer<S> {
    fn clone(&self) -> Self {
        Self {
            cqs: self.cqs.clone(),
            cqr: self.cqr.clone(),
        }
    }
}

with_send!(Submitter, sqs, Sender, SQE);
with_recv!(Submitter, sqr, Receiver, CQE);
with_send!(Completer, cqs, Sender, CQE);
with_recv!(Completer, cqr, Receiver, SQE);

pub fn channel<S: UringSpec>(cap: usize) -> Uring<S> {
    let (cqs, sqr) = async_channel::bounded(cap);
    let (sqs, cqr) = async_channel::bounded(cap);
    (Completer { cqs, cqr }, Submitter { sqs, sqr })
}


pub fn default_channel<S: UringSpec>() -> Uring<S> {
    channel(crate::uring::DEFAULT_CAP)
}
