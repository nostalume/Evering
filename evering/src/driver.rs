use alloc::sync::Arc;
use lock_api::{Mutex, RawMutex};
use slab::Slab;

use core::marker::PhantomData;
use core::mem;
use core::ops::{Deref, DerefMut};
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::{Context, LocalWaker, Poll};

use crate::uring::{Submitter, Uring, UringSpec, WithRecv, WithSend};

struct SpinDriver;

impl DriverSpec for SpinDriver {
    type Lock = spin::Mutex<()>;
}

pub struct Op<S: UringSpec, D: DriverSpec> {
    id: OpId,
    driver: Driver<S, D>,
}

impl<S: UringSpec, D: DriverSpec> Future for Op<S, D> {
    type Output = S::CQE;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut core = self.driver.lock();
        let op_sign = core.ops.get_mut(self.id.into()).unwrap();

        match &mut op_sign.state {
            OpSignState::Completed(_) => {
                let OpSign {
                    state: OpSignState::Completed((_, payload)),
                    cancelled: _,
                } = core.ops.remove(self.id.into())
                else {
                    unreachable!()
                };
                Poll::Ready(payload)
            }
            OpSignState::Waiting(waker) => {
                if !waker.will_wake(cx.local_waker()) {
                    *waker = cx.local_waker().clone();
                }
                Poll::Pending
            }
        }
    }
}

impl<S: UringSpec, D: DriverSpec> Drop for Op<S, D> {
    fn drop(&mut self) {
        let core = self.driver.lock();
        if let Some(op_sign) = core.ops.get(self.id.into()) {
            op_sign.cancelled.store(true, Ordering::Release);
        }
    }
}

type OpId = usize;
type OpSigns<T> = Slab<OpSign<T>>;
struct OpSign<T> {
    state: OpSignState<T>,
    cancelled: AtomicBool,
}

enum OpSignState<T> {
    Waiting(LocalWaker),
    Completed(T),
    // Cancelled(#[allow(dead_code)] Cancellation),
}

impl<T> OpSign<T> {
    fn init() -> Self {
        OpSign {
            state: OpSignState::Waiting(LocalWaker::noop().clone()),
            cancelled: AtomicBool::new(false),
        }
    }

    fn complete(&mut self, completed: T) {
        match mem::replace(&mut self.state, OpSignState::Completed(completed)) {
            OpSignState::Waiting(waker) => waker.wake(),
            OpSignState::Completed(_) => (),
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

pub trait DriverSpec {
    type Lock: RawMutex;
}

struct DriverUring<S: UringSpec> {
    _marker: PhantomData<S>,
}

impl<S: UringSpec> UringSpec for DriverUring<S> {
    type SQE = (OpId, S::SQE);
    type CQE = (OpId, S::CQE);
}

struct DriverCore<S: UringSpec> {
    ops: OpSigns<S::CQE>,
}

struct Driver<S: UringSpec, D: DriverSpec> {
    inner: Arc<Mutex<D::Lock, DriverCore<DriverUring<S>>>>,
}

impl<S: UringSpec, D: DriverSpec> Deref for Driver<S, D> {
    type Target = Arc<Mutex<D::Lock, DriverCore<DriverUring<S>>>>;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<S: UringSpec, D: DriverSpec> Clone for Driver<S, D> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

struct Bridge<S: UringSpec, D: DriverSpec, R: Role> {
    driver: Driver<S, D>,
    sq: Submitter<DriverUring<S>>,
    _marker: PhantomData<R>,
}

trait Role {}

struct Submit;
impl Role for Submit {}
struct Receive;
impl Role for Receive {}

pub type SubmitterBridge<S, D> = Bridge<S, D, Submit>;
pub type ReceiverBridge<S, D> = Bridge<S, D, Receive>;

impl<S: UringSpec, D: DriverSpec> Bridge<S, D, Submit> {
    pub fn submit(&mut self, data: <S as UringSpec>::SQE) -> Op<S, D> {
        let mut core = self.driver.inner.lock();

        let id = core.ops.insert(OpSign::init());
        let op = Op {
            id,
            driver: self.driver.clone(),
        };

        let sqe = (id, data);
        self.sq.sender().send(sqe).ok().unwrap();

        op
    }
}

impl<S: UringSpec, D: DriverSpec> Bridge<S, D, Receive> {
    pub fn recv_op(&mut self) {
        for (id, payload) in self.sq.recv_bulk() {
            let mut core = self.driver.inner.lock();
            if let Some(op_sign) = core.ops.get_mut(id.into()) {
                if op_sign.cancelled() {
                    core.ops.remove(id.into());
                    continue;
                }

                op_sign.complete((id, payload));
                core.ops.remove(id.into());
            }
        }
    }
}

// impl<P, Ext> Drop for DriverInner<P, Ext> {
//     fn drop(&mut self) {
//         assert!(
//             self.ops
//                 .iter()
//                 .all(|(_, op)| matches!(op.state, OpSignState::Completed(_))),
//             "all operations inside `Driver` must be completed before dropping"
//         );
//     }
// }

// pub trait DriverHandle: 'static + Unpin {
//     type Payload;
//     type Ext;
//     type Ref: core::ops::Deref<Target = Driver<Self::Payload, Self::Ext>>;

//     fn get(&self) -> Self::Ref;
// }
// impl<P, Ext> DriverHandle for alloc::rc::Weak<Driver<P, Ext>>
// where
//     P: 'static,
//     Ext: 'static,
// {
//     type Payload = P;
//     type Ext = Ext;
//     type Ref = alloc::rc::Rc<Driver<P, Ext>>;
//     fn get(&self) -> Self::Ref {
//         self.upgrade().expect("not inside a valid executor")
//     }
// }

// pub unsafe trait Completable: 'static + Unpin {
//     type Output;
//     type Driver: DriverHandle;

//     /// Transforms the received payload to the corresponding output.
//     ///
//     /// This function is called when the operation is completed, and the output
//     /// is then returned as [`Poll::Ready`].
//     fn complete(
//         self,
//         driver: &Self::Driver,
//         payload: <Self::Driver as DriverHandle>::Payload,
//     ) -> Self::Output;

//     /// Completes this operation with the submitted extension.
//     ///
//     /// For more information, see [`complete`](Self::complete).
//     fn complete_ext(
//         self,
//         driver: &Self::Driver,
//         payload: <Self::Driver as DriverHandle>::Payload,
//         ext: <Self::Driver as DriverHandle>::Ext,
//     ) -> Self::Output
//     where
//         Self: Sized,
//     {
//         _ = ext;
//         self.complete(driver, payload)
//     }

//     /// Cancels this operation.
//     fn cancel(self, driver: &Self::Driver) -> Cancellation;
// }

// pub struct Cancellation(#[allow(dead_code)] Option<Box<dyn Any>>);

// impl Cancellation {
//     pub const fn noop() -> Self {
//         Self(None)
//     }

//     pub fn recycle<T: 'static>(resource: T) -> Self {
//         Self(Some(Box::new(resource)))
//     }
// }
