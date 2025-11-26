use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    panic::{RefUnwindSafe, UnwindSafe},
    sync::atomic::AtomicUsize,
};

use crate::channel::{Header, Slot, Slots};

struct Queue<T> {
    h: Header,
    buf: Box<Slots<T>>,
}

unsafe impl<T: Send> Send for Queue<T> {}
unsafe impl<T: Send> Sync for Queue<T> {}
impl<T> UnwindSafe for Queue<T> {}
impl<T> RefUnwindSafe for Queue<T> {}

struct QueueHandle<'a, T> {
    h: &'a Header,
    buf: &'a Slots<T>,
}

impl<T> core::fmt::Debug for QueueHandle<'_, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.pad("QueueHandle { .. }")
    }
}

unsafe impl<T: Send> Send for QueueHandle<'_, T> {}
unsafe impl<T: Send> Sync for QueueHandle<'_, T> {}
impl<T> UnwindSafe for QueueHandle<'_, T> {}
impl<T> RefUnwindSafe for QueueHandle<'_, T> {}

impl<T> Drop for Queue<T> {
    fn drop(&mut self) {
        use crate::channel::QueueDrop;
        let handle = self.handle();
        unsafe { handle.drop_in() };
    }
}

impl<T> Queue<T> {
    fn new(cap: usize) -> Self {
        let h = Header::new(cap);
        // Allocate a buffer of `cap` slots initialized
        // with stamps.
        let buf: Box<Slots<T>> = (0..cap)
            .map(|i| {
                // Set the stamp to `{ lap: 0, index: i }`.
                Slot {
                    stamp: AtomicUsize::new(i),
                    value: UnsafeCell::new(MaybeUninit::uninit()),
                }
            })
            .collect();

        Self { h, buf }
    }
    fn handle(&self) -> QueueHandle<'_, T> {
        QueueHandle {
            h: &self.h,
            buf: &self.buf,
        }
    }
}

impl<T> super::Queue for QueueHandle<'_, T> {
    type Item = T;

    #[inline]
    fn header(&self) -> &Header {
        self.h
    }

    #[inline]
    fn buf(&self) -> &Slots<Self::Item> {
        self.buf
    }
}

#[cfg(test)]
mod tests {
    use super::super::QueueOps;
    use super::Queue;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread::scope;

    #[test]
    fn smoke() {
        let q = Queue::new(1);
        let handle = q.handle();

        handle.push(7).unwrap();
        assert_eq!(handle.pop(), Some(7));

        handle.push(8).unwrap();
        assert_eq!(handle.pop(), Some(8));
        assert!(handle.pop().is_none())
    }

    #[test]
    fn capacity() {
        for i in 1..10 {
            let q = Queue::<i32>::new(i);
            assert_eq!(q.handle().capacity(), i)
        }
    }

    #[test]
    #[should_panic]
    fn zero_capacity() {
        let _ = Queue::<i32>::new(0);
    }

    #[test]
    fn mpmc_ring_buffer() {
        #[cfg(miri)]
        const COUNT: usize = 50;
        #[cfg(not(miri))]
        const COUNT: usize = 25_000;
        const THREADS: usize = 4;

        let t = AtomicUsize::new(THREADS);
        let q = Queue::<usize>::new(3);
        let v = (0..COUNT).map(|_| AtomicUsize::new(0)).collect::<Vec<_>>();

        scope(|scope| {
            for _ in 0..THREADS {
                scope.spawn(|| {
                    loop {
                        match t.load(Ordering::SeqCst) {
                            0 if q.handle().is_empty() => break,

                            _ => {
                                while let Some(n) = q.handle().pop() {
                                    v[n].fetch_add(1, Ordering::SeqCst);
                                }
                            }
                        }
                    }
                });
            }

            for _ in 0..THREADS {
                scope.spawn(|| {
                    for i in 0..COUNT {
                        if let Some(n) = q.handle().force_push(i) {
                            v[n].fetch_add(1, Ordering::SeqCst);
                        }
                    }

                    t.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });

        for c in v {
            assert_eq!(c.load(Ordering::SeqCst), THREADS);
        }
    }
}
