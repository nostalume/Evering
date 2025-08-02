use core::{mem, task::Waker};

#[derive(Debug, Default)]
pub struct OpCache<T> {
    pub state: OpCacheState<T>,
}

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

impl<T> OpCache<T> {
    pub fn init() -> Self {
        OpCache {
            state: OpCacheState::Waiting(Waker::noop().clone()),
        }
    }

    pub fn complete(&mut self, completed: T) {
        match mem::replace(&mut self.state, OpCacheState::Completed(completed)) {
            OpCacheState::Waiting(waker) => waker.wake(),
            OpCacheState::Completed(_) => (),
        }
    }
}
