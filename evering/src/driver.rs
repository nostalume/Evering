use alloc::sync::Arc;
use lock_api::{Mutex, RawMutex};
use slab::Slab;

use core::marker::PhantomData;
use core::mem;
use core::ops::{Deref, DerefMut};
use core::pin::Pin;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::{Context, LocalWaker, Poll};

use crate::uring::{RawUring, UringA, UringB, UringReceiver, UringSender, UringSpec};

pub struct Op<S: DriverSpec> {
    id: OpId,
    driver: Driver<S>,
}

impl<S: DriverSpec> Future for Op<S> {
    type Output = S::B;

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

impl<S: DriverSpec> Drop for Op<S> {
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


pub trait DriverSpec : UringSpec {
    type Lock: RawMutex;
}

struct DriverUring<S: UringSpec> {
    _marker: PhantomData<S>,
}

impl<S: UringSpec> UringSpec for DriverUring<S> {
    type A = (OpId, S::A);
    type B = (OpId, S::B);
    type Ext = S::Ext;
}


struct DriverCore<S: UringSpec> {
    ops: OpSigns<S::B>,
}

struct Driver<S: DriverSpec> {
    inner: Arc<Mutex<S::Lock, DriverCore<DriverUring<S>>>>,
}

impl<S:DriverSpec> Deref for Driver<S> {
    type Target = Arc<Mutex<S::Lock, DriverCore<DriverUring<S>>>>;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<S:DriverSpec> Clone for Driver<S> {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

struct Bridge<S: DriverSpec, R: Role> {
    driver: Driver<S>,
    sq: UringA<DriverUring<S>>,
    _marker: PhantomData<R>,
}

trait Role {}

struct Submitter;
impl Role for Submitter {}
struct Receiver;
impl Role for Receiver {}

pub type SubmitterBridge<S> = Bridge<S, Submitter>;
pub type ReceiverBridge<S> = Bridge<S, Receiver>;


impl<S: DriverSpec> Bridge<S, Submitter> {
    pub fn submit(&mut self, data: <S as UringSpec>::A) -> Op<S> {
        let mut core = self.driver.inner.lock();

        let id = core.ops.insert(OpSign::init());
        let op = Op {
            id,
            driver: self.driver.clone(),
        };

        let sqe = (id, data);
        self.sq.send(sqe).ok().unwrap();

        op
    }
}

impl<S: DriverSpec> Bridge<S, Receiver> {
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
