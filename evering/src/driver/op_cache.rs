use core::{
    mem,
    task::{Context, Poll, Waker},
};

#[derive(Debug)]
pub enum OpCacheState<T> {
    Waiting(Waker),
    Completed(T),
}

impl<T> Default for OpCacheState<T> {
    fn default() -> Self {
        OpCacheState::Waiting(Waker::noop().clone())
    }
}

impl<T> OpCacheState<T> {
    pub fn init() -> Self {
        OpCacheState::Waiting(Waker::noop().clone())
    }

    pub fn try_complete(&mut self, completed: T) -> bool {
        match mem::replace(self, OpCacheState::Completed(completed)) {
            OpCacheState::Waiting(waker) => {
                waker.wake();
                true
            }
            OpCacheState::Completed(_) => false,
        }
    }

    pub fn try_poll(&mut self, cx: &mut Context<'_>) -> Poll<T> {
        match self {
            OpCacheState::Completed(_) => {
                let OpCacheState::Completed(payload) = mem::take(self) else {
                    unreachable!()
                };
                Poll::Ready(payload)
            }
            OpCacheState::Waiting(waker) => {
                if !waker.will_wake(cx.waker()) {
                    *waker = cx.waker().clone();
                }
                Poll::Pending
            }
        }
    }

    pub fn clean(&mut self) {
        mem::take(self);
    }
}
