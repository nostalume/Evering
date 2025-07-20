use core::{
    alloc::Layout,
    cell::UnsafeCell,
    fmt,
    mem::{self, MaybeUninit},
    ptr::NonNull,
    sync::atomic::{self, AtomicUsize, Ordering},
};

use crossbeam_utils::{Backoff, CachePadded};

/// A slot with stamp to achieve atomic access
pub struct Slot<T> {
    /// The stamp of the slot
    stamp: AtomicUsize,
    /// The value of the slot
    value: UnsafeCell<MaybeUninit<T>>,
}

impl<T> Slot<T> {
    #[inline]
    unsafe fn write(&self, val: T) {
        unsafe { self.value.get().write(MaybeUninit::new(val)) }
    }

    #[inline]
    unsafe fn read(&self) -> T {
        unsafe { self.value.get().read().assume_init() }
    }
}

pub type Head = usize;
pub type Tail = usize;
pub type Idx = usize;
pub type Lap = usize;

pub struct Queue<'a, T> {
    range: &'a mut Range,
    buf: NonNull<Slot<T>>,
}

impl<'a, T> Queue<'a, T> {
    /// Initializes a new queue.
    ///
    /// # Safety
    ///
    /// The operation is `unsafe` which guarantee **no** memory verification.
    ///
    /// User should ensure the correct memory layout with correct range.
    pub unsafe fn init(range: &'a mut Range, buf: NonNull<Slot<T>>) -> Queue<'a, T> {
        Queue { range, buf }
    }
    /// Returns the capacity of the queue.
    #[inline(always)]
    pub fn capacity(&self) -> usize {
        self.range.capacity()
    }

    /// Returns the length of the queue.
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.range.len()
    }

    /// Returns `true` if the queue is empty.
    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.range.is_empty()
    }

    /// Returns `true` if the queue is full.
    #[inline(always)]
    pub fn is_full(&self) -> bool {
        self.range.is_full()
    }

    #[inline(always)]
    fn one_lap(&self) -> usize {
        self.range.one_lap
    }

    #[inline(always)]
    unsafe fn get_unchecked(&self, idx: usize) -> &Slot<T> {
        unsafe { self.buf.add(idx).as_ref() }
    }

    fn enqueue_or_else<F>(&self, val: T, f: F) -> Result<(), T>
    where
        F: Fn(T, Tail, Tail, &Slot<T>) -> Result<T, T>,
    {
        let mut val = val;
        let backoff = Backoff::new();

        loop {
            let (tail, idx, lap) = self.range.cur_tail();

            debug_assert!(idx < self.capacity());
            let slot = unsafe { self.get_unchecked(idx) };
            let stamp = slot.stamp.load(Ordering::Acquire);

            let new_tail = if idx + 1 < self.capacity() {
                // lap: lap; idx: idx + 1
                tail + 1
            } else {
                // lap: lap + 1; idx: 0
                lap.wrapping_add(self.one_lap())
            };

            if tail == stamp {
                match self.range.tail.compare_exchange_weak(
                    tail,
                    new_tail,
                    Ordering::SeqCst,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        unsafe { slot.write(val) };
                        slot.stamp.store(tail + 1, Ordering::Release);
                        return Ok(());
                    }
                    Err(_) => {
                        backoff.spin();
                    }
                }
            } else if stamp.wrapping_add(self.range.one_lap) == tail + 1 {
                atomic::fence(Ordering::SeqCst);
                val = f(val, tail, new_tail, slot)?;
                backoff.spin();
            } else {
                backoff.snooze();
            }
        }
    }

    /// Attempts to enqueue a value into the queue.
    ///
    /// If the queue is full, the value is returned as an error.
    pub fn enqueue(&self, val: T) -> Result<(), T> {
        self.enqueue_or_else(val, |v, tail, _, _| {
            let head = self.range.head.load(Ordering::Relaxed);

            if head.wrapping_add(self.one_lap()) == tail {
                Err(v)
            } else {
                Ok(v)
            }
        })
    }

    /// Enqueue an element, replacing the oldest value if the queue is full.
    ///
    /// If the queue is full, the oldest value is returned as an error.
    /// Otherwise, `None` is returned.
    pub fn force_enqueue(&self, val: T) -> Option<T> {
        self.enqueue_or_else(val, |v, tail, new_tail, slot| {
            let head = tail.wrapping_sub(self.one_lap());
            let new_head = new_tail.wrapping_sub(self.one_lap());

            if self
                .range
                .head
                .compare_exchange_weak(head, new_head, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                self.range.tail.store(new_tail, Ordering::SeqCst);
                let old = unsafe { slot.value.get().replace(MaybeUninit::new(v)).assume_init() };
                slot.stamp.store(tail + 1, Ordering::Release);
                Err(old)
            } else {
                Ok(v)
            }
        })
        .err()
    }

    /// Enqueue a bulk of elements.
    ///
    /// Returns the number of successfully enqueued elements.
    pub fn enqueue_bulk(&mut self, vals: impl Iterator<Item = T>) -> usize {
        let mut vals = vals;

        let mut suc = 0;
        let suc = loop {
            let Some(val) = vals.next() else {
                break suc;
            };

            // Safety: The internal state is modified by a lock-free operation
            if let Err(_) = self.enqueue(val) {
                break suc;
            }

            suc += 1;
        };

        suc
    }

    /// Attempts to dequeue a value from the queue.
    ///
    /// If the queue is empty, `None` is returned.
    pub fn dequeue(&mut self) -> Option<T> {
        let backoff = Backoff::new();

        loop {
            let (head, idx, lap) = self.range.cur_head();

            debug_assert!(idx < self.capacity());
            let slot = unsafe { self.get_unchecked(idx) };
            let stamp = slot.stamp.load(Ordering::Acquire);

            if head + 1 == stamp {
                let new_head = if idx + 1 < self.capacity() {
                    head + 1
                } else {
                    lap.wrapping_add(self.one_lap())
                };

                match self.range.head.compare_exchange_weak(
                    head,
                    new_head,
                    Ordering::SeqCst,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        let res = unsafe { slot.read() };
                        // head + one_lap = tail
                        slot.stamp
                            .store(head.wrapping_add(self.one_lap()), Ordering::Release);
                        return Some(res);
                    }
                    Err(_) => {
                        backoff.spin();
                    }
                }
            } else if head == stamp {
                atomic::fence(Ordering::SeqCst);
                let tail = self.range.tail.load(Ordering::Relaxed);
                if tail == head {
                    return None;
                }
                backoff.spin();
            } else {
                backoff.snooze();
            }
        }
    }

    pub fn dequeue_bulk<'q>(&'q mut self) -> Drain<'q, 'a, T> {
        Drain { queue: self }
    }

    /// Drops all elements in the queue.
    ///
    /// # Safety
    ///
    /// The operation is `unsafe` which guarantee **no** atomic access.
    ///
    /// The queue must be owned by the **sole** instance.
    pub unsafe fn drop_elems(&mut self) {
        if mem::needs_drop::<T>() {
            let head = *self.range.head.get_mut();
            let tail = *self.range.tail.get_mut();

            let hix = head & (self.one_lap() - 1);
            let tix = tail & (self.one_lap() - 1);

            let len = if hix < tix {
                tix - hix
            } else if hix > tix {
                self.capacity() - tix + hix
            } else if tail == head {
                0
            } else {
                self.capacity()
            };

            for i in 0..len {
                let idx = if hix + 1 < self.capacity() {
                    hix + i
                } else {
                    hix + i - self.capacity()
                };

                unsafe {
                    debug_assert!(idx < self.capacity());
                    let slot = self.get_unchecked(idx);
                    (*slot.value.get()).assume_init_drop();
                }
            }
        }
    }
}

impl<'a, T> fmt::Debug for Queue<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cap = self.capacity();
        let len = self.len();
        f.debug_struct("Queue")
            .field("cap", &cap)
            .field("len", &len)
            .finish()
    }
}

pub struct Drain<'q, 'a, T> {
    queue: &'q mut Queue<'a, T>,
}

impl<'q, 'a, T> Iterator for Drain<'q, 'a, T> {
    type Item = T;
    fn next(&mut self) -> Option<Self::Item> {
        let queue = &mut self.queue;
        let head = *queue.range.head.get_mut();
        // Safety: The mutable access is exlucisve in concurrent use.
        // It's known that the queue is not empty.
        if queue.range.head.get_mut() != queue.range.tail.get_mut() {
            let idx = head & (queue.one_lap() - 1);
            let lap = head & !(queue.one_lap() - 1);

            let val = unsafe {
                debug_assert!(idx < queue.capacity());
                let slot = queue.get_unchecked(idx);
                slot.read()
            };
            let new = if idx + 1 < queue.capacity() {
                head + 1
            } else {
                lap.wrapping_add(queue.one_lap())
            };
            *queue.range.head.get_mut() = new;
            Some(val)
        } else {
            None
        }
    }
}

pub struct Range {
    head: CachePadded<AtomicUsize>,
    tail: CachePadded<AtomicUsize>,
    cap: usize,
    one_lap: usize,
}

impl Range {
    /// Creates a new range.
    ///
    /// # Examples
    ///
    /// ```
    /// use evering::uring::Pow2;
    /// use evering::uring::queue::Range;
    ///
    /// let range = Range::new(Pow2::new(1024));
    /// assert_eq!(range.capacity(), 1024);
    /// ```
    pub fn new(cap: Pow2) -> Self {
        // Safety: `Pow2` is always a power of 2 and greater than 1.
        let cap = cap.as_usize();
        // ideally it should return original cap.
        let one_lap = cap.next_power_of_two();
        Self {
            head: CachePadded::new(AtomicUsize::new(0)),
            tail: CachePadded::new(AtomicUsize::new(0)),
            cap,
            one_lap,
        }
    }

    /// Returns `true` if the range is empty.
    ///
    /// # Examples
    ///
    /// ```
    /// use evering::uring::queue::Range;
    ///
    /// let range = Range::new(Pow2::new(1024));
    /// assert!(range.is_empty());
    /// ```
    #[inline]
    pub fn is_empty(&self) -> bool {
        // If the head changes before we load tail, it means the queue was not empty
        // so it's safe to return `false` "not empty".
        let head = self.head.load(Ordering::SeqCst);
        let tail = self.tail.load(Ordering::SeqCst);

        tail == head
    }

    /// Returns `true` if the range is full.
    ///
    /// # Examples
    ///
    /// ```
    /// use evering::uring::queue::Range;
    ///
    /// let range = Range::new(Pow2::new(1));
    /// assert!(!range.is_full());
    /// ```
    #[inline]
    pub fn is_full(&self) -> bool {
        // If the tail changes before we load head, it means the queue was not full
        // so it's safe to return `false` "not full".
        let tail = self.tail.load(Ordering::SeqCst);
        let head = self.head.load(Ordering::SeqCst);

        head.wrapping_add(self.one_lap) == tail
    }

    /// Returns the capacity of the range.
    ///
    /// # Examples
    ///
    /// ```
    /// use evering::uring::Pow2;
    /// use evering::uring::queue::Range;
    ///
    /// let range = Range::new(Pow2::new(1024));
    /// assert_eq!(range.capacity(), 1024);
    /// ```
    #[inline]
    pub fn capacity(&self) -> usize {
        self.cap
    }

    /// Returns the number of elements in the range.
    ///
    /// # Examples
    ///
    /// ```
    /// use evering::uring::Pow2;
    /// use evering::uring::queue::Range;
    ///
    /// let range = Range::new(Pow2::new(1024));
    /// assert_eq!(range.len(), 0);
    /// ```
    pub fn len(&self) -> usize {
        loop {
            let tail = self.tail.load(Ordering::SeqCst);
            let head = self.head.load(Ordering::SeqCst);

            // If the tail didn't change, the value is consistent:
            if self.tail.load(Ordering::SeqCst) == tail {
                let hix = head & (self.one_lap - 1);
                let tix = tail & (self.one_lap - 1);

                return if hix < tix {
                    tix - hix
                } else if hix > tix {
                    self.cap - hix + tix
                } else if tail == head {
                    // Same lap
                    0
                } else {
                    // Different lap, full
                    self.cap
                };
            }
        }
    }

    #[inline]
    pub fn cur_head(&self) -> (Head, Idx, Lap) {
        let head = self.head.load(Ordering::Relaxed);
        let idx = head & (self.one_lap - 1);
        let lap = head & !(self.one_lap - 1);

        (head, idx, lap)
    }

    #[inline]
    pub fn cur_tail(&self) -> (Head, Idx, Lap) {
        let tail = self.tail.load(Ordering::Relaxed);
        let idx = tail & (self.one_lap - 1);
        let lap = tail & !(self.one_lap - 1);

        (tail, idx, lap)
    }
}

#[derive(Clone, Copy)]
pub struct Pow2(usize);

impl Pow2 {
    pub const fn new(pow: usize) -> Self {
        // compile time check
        assert!(pow < 32, "pow should be less than 32");
        Self(1 << pow)
    }

    pub const fn as_usize(self) -> usize {
        self.0
    }
}
