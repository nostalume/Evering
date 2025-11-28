use core::{
    ops::Deref,
    sync::atomic::{AtomicUsize, Ordering},
};

struct Counter<T> {
    counts: AtomicUsize,
    data: T,
}

pub struct CounterOf<T> {
    counter: *mut Counter<T>,
}

impl<T> CounterOf<T> {
    pub fn suspend(data: T) -> Self {
        let counter = Box::into_raw(Box::new(Counter {
            counts: AtomicUsize::new(1),
            data,
        }));
        Self { counter }
    }

    fn counter(&self) -> &Counter<T> {
        unsafe { &*self.counter }
    }

    fn as_raw(&self) -> *mut T {
        let counter = unsafe { &mut *self.counter };
        &mut counter.data as *mut T
    }

    pub fn acquire(&self) -> Self {
        let count = self.counter().counts.fetch_add(1, Ordering::Relaxed);

        // Cloning senders and calling `mem::forget` on the clones could potentially overflow the
        // counter. It's very difficult to recover sensibly from such degenerate scenarios so we
        // just abort when the count becomes very large.
        if count > isize::MAX as usize {
            core::panic!("counts exceed `isize::MAX`")
        }

        Self {
            counter: self.counter,
        }
    }

    pub unsafe fn release<F: FnOnce(*mut T)>(&self, dispose: F) {
        if self.counter().counts.fetch_sub(1, Ordering::AcqRel) == 1 {
            dispose(self.as_raw());
            drop(unsafe { Box::from_raw(self.counter) });
        }
    }
    
    pub unsafe fn release_of(&self) {
        if self.counter().counts.fetch_sub(1, Ordering::AcqRel) == 1 {
            unsafe { core::ptr::drop_in_place(self.as_raw()) };
            drop(unsafe { Box::from_raw(self.counter) });
        }
    }
}

impl<T> Deref for CounterOf<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.counter().data
    }
}

impl<T> PartialEq for CounterOf<T> {
    fn eq(&self, other: &CounterOf<T>) -> bool {
        self.counter == other.counter
    }
}
