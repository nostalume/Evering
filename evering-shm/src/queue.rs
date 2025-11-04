use core::cell::UnsafeCell;
use core::fmt;
use core::mem::MaybeUninit;
use core::panic::{RefUnwindSafe, UnwindSafe};
use core::sync::atomic::{self, AtomicUsize, Ordering};

use crossbeam_utils::{Backoff, CachePadded};

type Slots<T> = [Slot<T>];
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

struct BoxQueue<T> {
    h: Header,
    buf: Box<Slots<T>>,
}
unsafe impl<T: Send> Send for BoxQueue<T> {}
unsafe impl<T: Send> Sync for BoxQueue<T> {}
impl<T> UnwindSafe for BoxQueue<T> {}
impl<T> RefUnwindSafe for BoxQueue<T> {}

struct BoxQueueHandle<'a, T> {
    h: &'a Header,
    buf: &'a Slots<T>,
}

impl<T> fmt::Debug for BoxQueueHandle<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("QueueHandle { .. }")
    }
}

unsafe impl<T: Send> Send for BoxQueueHandle<'_, T> {}
unsafe impl<T: Send> Sync for BoxQueueHandle<'_, T> {}
impl<T> UnwindSafe for BoxQueueHandle<'_, T> {}
impl<T> RefUnwindSafe for BoxQueueHandle<'_, T> {}

use crate::boxed::PBox;
pub(crate) use crate::malloc::{MemAllocator, MetaSpanOf};
pub(crate) use crate::msg::{Envelope, PackToken, TokenOf};
type TokenSlots<H, M> = Slots<PackToken<H, M>>;
type TokenOfSlots<H, M> = TokenOf<TokenSlots<H, M>, M>;
struct TokenQueue<H: Envelope, A: MemAllocator> {
    h: Header,
    buf: TokenOfSlots<H, MetaSpanOf<A>>,
}
unsafe impl<H: Send + Envelope, A: MemAllocator> Send for TokenQueue<H, A> {}
unsafe impl<H: Send + Envelope, A: MemAllocator> Sync for TokenQueue<H, A> {}
impl<H: Envelope, A: MemAllocator> UnwindSafe for TokenQueue<H, A> {}
impl<H: Envelope, A: MemAllocator> RefUnwindSafe for TokenQueue<H, A> {}

pub(crate) use crate::reg::{Entry, EntryGuard};
type QEntry<H, A> = Entry<TokenQueue<H, A>>;
type QEntryGuard<'a, H, A> = EntryGuard<'a, TokenQueue<H, A>>;
struct TokenQueueHandle<'a, H: Envelope, A: MemAllocator> {
    g: QEntryGuard<'a, H, A>,
    buf: &'a TokenSlots<H, MetaSpanOf<A>>,
}

impl<T> BoxQueue<T> {
    fn new(cap: usize) -> Self {
        let h = Header::new(cap);
        // Allocate a buffer of `cap` slots initialized
        // with stamps.
        let buf: Box<[Slot<T>]> = (0..cap)
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
    fn handle(&self) -> BoxQueueHandle<'_, T> {
        BoxQueueHandle {
            h: &self.h,
            buf: &self.buf,
        }
    }
}

impl<T> QueueOps for BoxQueueHandle<'_, T> {
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

impl<H: Envelope, A: MemAllocator> TokenQueue<H, A> {
    pub fn new(cap: usize, alloc: A) -> (Self, A) {
        let h = Header::new(cap);
        let buffer: PBox<_, A> = PBox::new_slice_in(
            cap,
            |i| Slot {
                stamp: AtomicUsize::new(i),
                value: UnsafeCell::new(MaybeUninit::uninit()),
            },
            alloc,
        );
        let (buf, alloc) = buffer.token_with();
        (TokenQueue { h, buf }, alloc)
    }

    /// Drop the slots with explicit allocator handle.
    ///
    /// Otherwise the slot won't be deallocated.
    pub fn drop_in(self, alloc: A) {
        let Self { h: _, buf } = self;
        let b = PBox::<[_], A>::detoken(buf, alloc);
        drop(b)
    }
}

impl<'a, H: Envelope, A: MemAllocator> TokenQueueHandle<'a, H, A> {
    fn from_guard(g: QEntryGuard<'a, H, A>, alloc: A) -> Self {
        let buf = unsafe { TokenOf::<[_], MetaSpanOf<A>>::as_ptr(&g.buf, alloc).as_ref() };
        Self { g, buf }
    }
}

impl<H: Envelope, A: MemAllocator> QEntry<H, A> {
    fn qinit(s: QEntry<H, A>, cap: usize, alloc: A) -> Result<(), usize> {
        let (q, alloc) = TokenQueue::<H, A>::new(cap, alloc);
        s.init(q).map_err(|q| {
            q.drop_in(alloc);
            cap
        })
    }

    fn qreset(&self, alloc: A) {
        self.reset(|q| q.drop_in(alloc));
    }

    fn qacquire<'a>(&'a self, alloc: A) -> Option<TokenQueueHandle<'a, H, A>> {
        if let Some(g) = self.acquire() {
            Some(TokenQueueHandle::from_guard(g, alloc))
        } else {
            None
        }
    }
}

trait QueueOps {
    type Item;

    fn header(&self) -> &Header;
    fn buf(&self) -> &Slots<Self::Item>;

    /// Attempts to push an element into the queue.
    fn push(&self, value: Self::Item) -> Result<(), Self::Item> {
        let header = self.header();
        self.push_or_else(value, |v, tail, _, _| {
            let head = header.head.load(Ordering::Relaxed);

            // If the head lags one lap behind the tail as well...
            if head.wrapping_add(header.one_lap) == tail {
                // ...then the queue is full.
                Err(v)
            } else {
                Ok(v)
            }
        })
    }

    /// Pushes an element into the queue, replacing the oldest element if necessary.
    fn force_push(&self, value: Self::Item) -> Option<Self::Item> {
        self.push_or_else(value, |v, tail, new_tail, slot| {
            let header = self.header();
            let head = tail.wrapping_sub(header.one_lap);
            let new_head = new_tail.wrapping_sub(header.one_lap);

            // Try moving the head.
            if header
                .head
                .compare_exchange_weak(head, new_head, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                // Move the tail.
                header.tail.store(new_tail, Ordering::SeqCst);

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

    fn push_or_else<F>(&self, value: Self::Item, f: F) -> Result<(), Self::Item>
    where
        F: Fn(Self::Item, usize, usize, &Slot<Self::Item>) -> Result<Self::Item, Self::Item>,
    {
        let header = self.header();
        let mut tail = header.tail.load(Ordering::Relaxed);
        let buf = self.buf();
        let mut value = value;

        let backoff = Backoff::new();

        loop {
            // Deconstruct the tail.
            let index = tail & (header.one_lap - 1);
            let lap = tail & !(header.one_lap - 1);

            let new_tail = if index + 1 < self.capacity() {
                // Same lap, incremented index.
                // Set to `{ lap: lap, index: index + 1 }`.
                tail + 1
            } else {
                // One lap forward, index wraps around to zero.
                // Set to `{ lap: lap.wrapping_add(1), index: 0 }`.
                lap.wrapping_add(header.one_lap)
            };

            // Inspect the corresponding slot.
            debug_assert!(index < buf.len());
            let slot = unsafe { buf.get_unchecked(index) };
            let stamp = slot.stamp.load(Ordering::Acquire);

            // If the tail and the stamp match, we may attempt to push.
            if tail == stamp {
                // Try moving the tail.
                match header.tail.compare_exchange_weak(
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
            } else if stamp.wrapping_add(header.one_lap) == tail + 1 {
                atomic::fence(Ordering::SeqCst);
                value = f(value, tail, new_tail, slot)?;
                backoff.spin();
                tail = header.tail.load(Ordering::Relaxed);
            } else {
                // Snooze because we need to wait for the stamp to get updated.
                backoff.snooze();
                tail = header.tail.load(Ordering::Relaxed);
            }
        }
    }

    /// Attempts to pop an element from the queue.
    fn pop(&self) -> Option<Self::Item> {
        let header = self.header();
        let mut head = header.head.load(Ordering::Relaxed);
        let buf = self.buf();

        let backoff = Backoff::new();

        loop {
            // Deconstruct the head.
            let index = head & (header.one_lap - 1);
            let lap = head & !(header.one_lap - 1);

            // Inspect the corresponding slot.
            debug_assert!(index < buf.len());
            let slot = unsafe { buf.get_unchecked(index) };
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
                    lap.wrapping_add(header.one_lap)
                };

                // Try moving the head.
                match header.head.compare_exchange_weak(
                    head,
                    new,
                    Ordering::SeqCst,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => {
                        // Read the value from the slot and update the stamp.
                        let msg = unsafe { slot.value.get().read().assume_init() };
                        slot.stamp
                            .store(head.wrapping_add(header.one_lap), Ordering::Release);
                        return Some(msg);
                    }
                    Err(h) => {
                        head = h;
                        backoff.spin();
                    }
                }
            } else if stamp == head {
                atomic::fence(Ordering::SeqCst);
                let tail = header.tail.load(Ordering::Relaxed);

                // If the tail equals the head, that means the channel is empty.
                if tail == head {
                    return None;
                }

                backoff.spin();
                head = header.head.load(Ordering::Relaxed);
            } else {
                // Snooze because we need to wait for the stamp to get updated.
                backoff.snooze();
                head = header.head.load(Ordering::Relaxed);
            }
        }
    }

    /// Returns the capacity of the queue.
    #[inline]
    fn capacity(&self) -> usize {
        self.buf().len()
    }

    /// Returns `true` if the queue is empty.
    fn is_empty(&self) -> bool {
        let header = self.header();
        let head = header.head.load(Ordering::SeqCst);
        let tail = header.tail.load(Ordering::SeqCst);

        // Is the tail lagging one lap behind head?
        // Is the tail equal to the head?
        //
        // Note: If the head changes just before we load the tail, that means there was a moment
        // when the channel was not empty, so it is safe to just return `false`.
        tail == head
    }

    /// Returns `true` if the queue is full.
    fn is_full(&self) -> bool {
        let header = self.header();
        let tail = header.tail.load(Ordering::SeqCst);
        let head = header.head.load(Ordering::SeqCst);

        // Is the head lagging one lap behind tail?
        //
        // Note: If the tail changes just before we load the head, that means there was a moment
        // when the queue was not full, so it is safe to just return `false`.
        head.wrapping_add(header.one_lap) == tail
    }

    /// Returns the number of elements in the queue.
    fn len(&self) -> usize {
        let header = self.header();
        loop {
            // Load the tail, then load the head.
            let tail = header.tail.load(Ordering::SeqCst);
            let head = header.head.load(Ordering::SeqCst);

            // If the tail didn't change, we've got consistent values to work with.
            if header.tail.load(Ordering::SeqCst) == tail {
                let hix = head & (header.one_lap - 1);
                let tix = tail & (header.one_lap - 1);

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
    use super::{BoxQueue, QueueOps};
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
