use core::cell::UnsafeCell;
use core::fmt;
use core::mem::MaybeUninit;
use core::ops;
use core::panic::{RefUnwindSafe, UnwindSafe};
use core::sync::atomic::{self, AtomicUsize, Ordering};

use crossbeam_utils::{Backoff, CachePadded};

/// A slot in a queue.
struct Slot<T> {
    /// The current stamp.
    ///
    /// If the stamp equals the tail, this node will be next written to. If it equals head + 1,
    /// this node will be next read from.
    stamp: AtomicUsize,

    /// The value in this slot.
    value: UnsafeCell<MaybeUninit<T>>,
}

type Slots<T> = [Slot<T>];

struct Queue<S> {
    h: Header,
    token: S,
}

type BoxQueue<T> = Queue<Box<Slots<T>>>;
unsafe impl<T: Send> Send for BoxQueue<T> {}
unsafe impl<T: Send> Sync for BoxQueue<T> {}
impl<T> UnwindSafe for BoxQueue<T> {}
impl<T> RefUnwindSafe for BoxQueue<T> {}

impl<T> BoxQueue<T> {
    fn new(cap: usize) -> Self {
        let h = Header::new(cap);
        // Allocate a buffer of `cap` slots initialized
        // with stamps.
        let buffer: Box<[Slot<T>]> = (0..cap)
            .map(|i| {
                // Set the stamp to `{ lap: 0, index: i }`.
                Slot {
                    stamp: AtomicUsize::new(i),
                    value: UnsafeCell::new(MaybeUninit::uninit()),
                }
            })
            .collect();

        Self { h, token: buffer }
    }
    fn handle(&self) -> QueueHandle<'_, T> {
        QueueHandle {
            h: &self.h,
            buffer: &self.token,
        }
    }
}

struct Header {
    /// The head of the queue.
    ///
    /// This value is a "stamp" consisting of an index into the buffer and a lap, but packed into a
    /// single `usize`. The lower bits represent the index, while the upper bits represent the lap.
    ///
    /// Elements are popped from the head of the queue.
    head: CachePadded<AtomicUsize>,

    /// The tail of the queue.
    ///
    /// This value is a "stamp" consisting of an index into the buffer and a lap, but packed into a
    /// single `usize`. The lower bits represent the index, while the upper bits represent the lap.
    ///
    /// Elements are pushed into the tail of the queue.
    tail: CachePadded<AtomicUsize>,

    /// A stamp with the value of `{ lap: 1, index: 0 }`.
    one_lap: usize,
}

impl Header {
    fn new(cap: usize) -> Self {
        assert!(cap > 0, "capacity must not zero");
        // Head is initialized to `{ lap: 0, index: 0 }`.
        // Tail is initialized to `{ lap: 0, index: 0 }`.
        let head = 0;
        let tail = 0;
        // One lap is the smallest power of two greater than `cap`.
        let one_lap = (cap + 1).next_power_of_two();
        let header = Header {
            head: CachePadded::new(AtomicUsize::new(head)),
            tail: CachePadded::new(AtomicUsize::new(tail)),
            one_lap,
        };
        header
    }
}

struct QueueHandle<'a, T> {
    h: &'a Header,
    buffer: &'a Slots<T>,
}

impl<T> ops::Deref for QueueHandle<'_, T> {
    type Target = Header;

    fn deref(&self) -> &Self::Target {
        self.h
    }
}

impl<T> fmt::Debug for QueueHandle<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("ArrayQueue { .. }")
    }
}

unsafe impl<T: Send> Send for QueueHandle<'_, T> {}
unsafe impl<T: Send> Sync for QueueHandle<'_, T> {}

impl<T> UnwindSafe for QueueHandle<'_, T> {}
impl<T> RefUnwindSafe for QueueHandle<'_, T> {}

impl<T> QueueHandle<'_, T> {
    /// Attempts to push an element into the queue.
    pub fn push(&self, value: T) -> Result<(), T> {
        self.push_or_else(value, |v, tail, _, _| {
            let head = self.head.load(Ordering::Relaxed);

            // If the head lags one lap behind the tail as well...
            if head.wrapping_add(self.one_lap) == tail {
                // ...then the queue is full.
                Err(v)
            } else {
                Ok(v)
            }
        })
    }

    /// Pushes an element into the queue, replacing the oldest element if necessary.
    pub fn force_push(&self, value: T) -> Option<T> {
        self.push_or_else(value, |v, tail, new_tail, slot| {
            let head = tail.wrapping_sub(self.one_lap);
            let new_head = new_tail.wrapping_sub(self.one_lap);

            // Try moving the head.
            if self
                .head
                .compare_exchange_weak(head, new_head, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                // Move the tail.
                self.tail.store(new_tail, Ordering::SeqCst);

                // Swap the previous value.
                let old = unsafe { slot.value.get().replace(MaybeUninit::new(v)).assume_init() };

                // Update the stamp.
                slot.stamp.store(tail + 1, Ordering::Release);

                Err(old)
            } else {
                Ok(v)
            }
        })
        .err()
    }

    fn push_or_else<F>(&self, value: T, f: F) -> Result<(), T>
    where
        F: Fn(T, usize, usize, &Slot<T>) -> Result<T, T>,
    {
        let mut value = value;
        let backoff = Backoff::new();
        let mut tail = self.tail.load(Ordering::Relaxed);

        loop {
            // Deconstruct the tail.
            let index = tail & (self.one_lap - 1);
            let lap = tail & !(self.one_lap - 1);

            let new_tail = if index + 1 < self.capacity() {
                // Same lap, incremented index.
                // Set to `{ lap: lap, index: index + 1 }`.
                tail + 1
            } else {
                // One lap forward, index wraps around to zero.
                // Set to `{ lap: lap.wrapping_add(1), index: 0 }`.
                lap.wrapping_add(self.one_lap)
            };

            // Inspect the corresponding slot.
            debug_assert!(index < self.buffer.len());
            let slot = unsafe { self.buffer.get_unchecked(index) };
            let stamp = slot.stamp.load(Ordering::Acquire);

            // If the tail and the stamp match, we may attempt to push.
            if tail == stamp {
                // Try moving the tail.
                match self.tail.compare_exchange_weak(
                    tail,
                    new_tail,
                    Ordering::SeqCst,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        // Write the value into the slot and update the stamp.
                        unsafe {
                            slot.value.get().write(MaybeUninit::new(value));
                        }
                        slot.stamp.store(tail + 1, Ordering::Release);
                        return Ok(());
                    }
                    Err(t) => {
                        tail = t;
                        backoff.spin();
                    }
                }
            } else if stamp.wrapping_add(self.one_lap) == tail + 1 {
                atomic::fence(Ordering::SeqCst);
                value = f(value, tail, new_tail, slot)?;
                backoff.spin();
                tail = self.tail.load(Ordering::Relaxed);
            } else {
                // Snooze because we need to wait for the stamp to get updated.
                backoff.snooze();
                tail = self.tail.load(Ordering::Relaxed);
            }
        }
    }

    /// Attempts to pop an element from the queue.
    pub fn pop(&self) -> Option<T> {
        let backoff = Backoff::new();
        let mut head = self.head.load(Ordering::Relaxed);

        loop {
            // Deconstruct the head.
            let index = head & (self.one_lap - 1);
            let lap = head & !(self.one_lap - 1);

            // Inspect the corresponding slot.
            debug_assert!(index < self.buffer.len());
            let slot = unsafe { self.buffer.get_unchecked(index) };
            let stamp = slot.stamp.load(Ordering::Acquire);

            // If the stamp is ahead of the head by 1, we may attempt to pop.
            if head + 1 == stamp {
                let new = if index + 1 < self.capacity() {
                    // Same lap, incremented index.
                    // Set to `{ lap: lap, index: index + 1 }`.
                    head + 1
                } else {
                    // One lap forward, index wraps around to zero.
                    // Set to `{ lap: lap.wrapping_add(1), index: 0 }`.
                    lap.wrapping_add(self.one_lap)
                };

                // Try moving the head.
                match self.head.compare_exchange_weak(
                    head,
                    new,
                    Ordering::SeqCst,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        // Read the value from the slot and update the stamp.
                        let msg = unsafe { slot.value.get().read().assume_init() };
                        slot.stamp
                            .store(head.wrapping_add(self.one_lap), Ordering::Release);
                        return Some(msg);
                    }
                    Err(h) => {
                        head = h;
                        backoff.spin();
                    }
                }
            } else if stamp == head {
                atomic::fence(Ordering::SeqCst);
                let tail = self.tail.load(Ordering::Relaxed);

                // If the tail equals the head, that means the channel is empty.
                if tail == head {
                    return None;
                }

                backoff.spin();
                head = self.head.load(Ordering::Relaxed);
            } else {
                // Snooze because we need to wait for the stamp to get updated.
                backoff.snooze();
                head = self.head.load(Ordering::Relaxed);
            }
        }
    }

    /// Returns the capacity of the queue.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.buffer.len()
    }

    /// Returns `true` if the queue is empty.
    pub fn is_empty(&self) -> bool {
        let head = self.head.load(Ordering::SeqCst);
        let tail = self.tail.load(Ordering::SeqCst);

        // Is the tail lagging one lap behind head?
        // Is the tail equal to the head?
        //
        // Note: If the head changes just before we load the tail, that means there was a moment
        // when the channel was not empty, so it is safe to just return `false`.
        tail == head
    }

    /// Returns `true` if the queue is full.
    pub fn is_full(&self) -> bool {
        let tail = self.tail.load(Ordering::SeqCst);
        let head = self.head.load(Ordering::SeqCst);

        // Is the head lagging one lap behind tail?
        //
        // Note: If the tail changes just before we load the head, that means there was a moment
        // when the queue was not full, so it is safe to just return `false`.
        head.wrapping_add(self.one_lap) == tail
    }

    /// Returns the number of elements in the queue.
    pub fn len(&self) -> usize {
        loop {
            // Load the tail, then load the head.
            let tail = self.tail.load(Ordering::SeqCst);
            let head = self.head.load(Ordering::SeqCst);

            // If the tail didn't change, we've got consistent values to work with.
            if self.tail.load(Ordering::SeqCst) == tail {
                let hix = head & (self.one_lap - 1);
                let tix = tail & (self.one_lap - 1);

                return if hix < tix {
                    tix - hix
                } else if hix > tix {
                    self.capacity() - hix + tix
                } else if tail == head {
                    0
                } else {
                    self.capacity()
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BoxQueue;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread::scope;

    #[test]
    fn smoke() {
        let q = BoxQueue::new(1);
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
            let q = BoxQueue::<i32>::new(i);
            assert_eq!(q.handle().capacity(), i)
        }
    }

    #[test]
    #[should_panic]
    fn zero_capacity() {
        let _ = BoxQueue::<i32>::new(0);
    }

    #[test]
    fn mpmc_ring_buffer() {
        #[cfg(miri)]
        const COUNT: usize = 50;
        #[cfg(not(miri))]
        const COUNT: usize = 25_000;
        const THREADS: usize = 4;

        let t = AtomicUsize::new(THREADS);
        let q = BoxQueue::<usize>::new(3);
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
