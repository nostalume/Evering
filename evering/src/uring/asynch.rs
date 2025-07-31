use async_channel::{Receiver, Sender};

use crate::uring::UringSpec;

pub trait WithSink {
    type T;
    fn sender(&self) -> &Sender<Self::T>;
}
pub trait WithStream {
    type T;
    fn receiver(&self) -> &Receiver<Self::T>;
}

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

impl<S: UringSpec> WithSink for Submitter<S> {
    type T = S::SQE;
    fn sender(&self) -> &Sender<Self::T> {
        &self.sqs
    }
}

impl<S: UringSpec> WithStream for Submitter<S> {
    type T = S::CQE;
    fn receiver(&self) -> &Receiver<Self::T> {
        &self.sqr
    }
}

impl<S: UringSpec> WithSink for Completer<S> {
    type T = S::CQE;
    fn sender(&self) -> &Sender<Self::T> {
        &self.cqs
    }
}

impl<S: UringSpec> WithStream for Completer<S> {
    type T = S::SQE;
    fn receiver(&self) -> &Receiver<Self::T> {
        &self.cqr
    }
}

pub fn channel<S: UringSpec>(cap: usize) -> Uring<S> {
    let (cqs, sqr) = async_channel::bounded(cap);
    let (sqs, cqr) = async_channel::bounded(cap);
    (Completer { cqs, cqr }, Submitter { sqs, sqr })
}

pub fn default_channel<S: UringSpec>() -> Uring<S> {
    let cap = 1 << 5;
    channel(cap)
}
