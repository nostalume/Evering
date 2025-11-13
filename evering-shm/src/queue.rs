use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::panic::{RefUnwindSafe, UnwindSafe};
use core::ptr;
use core::sync::atomic::{self, AtomicUsize, Ordering};

use crossbeam_utils::{Backoff, CachePadded};

type Slots<T> = [Slot<T>];
/// A slot in a queue.
pub struct Slot<T> {
    /// The current stamp.
    ///
    /// If the stamp equals the tail, this node will be next written to. If it equals head + 1,
    /// this node will be next read from.
    stamp: AtomicUsize,

    /// The value in this slot.
    value: UnsafeCell<MaybeUninit<T>>,
}

pub struct Header {
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

    /// The queue capacity.
    cap: usize,
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
            cap,
        };
        header
    }
}

pub trait Queue {
    type Item;

    fn header(&self) -> &Header;
    fn buf(&self) -> &Slots<Self::Item>;
}

pub trait QueueOps: Queue {
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

impl<T: Queue> QueueOps for T {}

trait QueueDrop: Queue {
    unsafe fn drop_in(&self) {
        if core::mem::needs_drop::<Self::Item>() {
            let header = self.header();
            let buf = self.buf();
            // Get the index of the head.
            let head = header.head.load(Ordering::Relaxed);
            let tail = header.tail.load(Ordering::Relaxed);

            let hix = head & (header.one_lap - 1);
            let tix = tail & (header.one_lap - 1);

            let len = if hix < tix {
                tix - hix
            } else if hix > tix {
                header.cap - hix + tix
            } else if tail == head {
                0
            } else {
                header.cap
            };

            // Loop over all slots that hold a message and drop them.
            for i in 0..len {
                // Compute the index of the next slot holding a message.
                let index = if hix + i < header.cap {
                    hix + i
                } else {
                    hix + i - header.cap
                };

                unsafe {
                    debug_assert!(index < buf.len());
                    let slot = buf.get_unchecked(index);
                    (*slot.value.get()).assume_init_drop();
                }
            }
        }
    }
}

impl<T: Queue> QueueDrop for T {}

use crate::boxed::PBox;
use crate::malloc::MemAllocator;
use crate::msg::{Envelope, SpanPackToken, SpanTokenOf};
use crate::reg::{EntryView, Project, Resource};
// Token needs to be transferred
type Tokens<H, A> = Slots<SpanPackToken<H, A>>;
// Token of the token slots
type TokenOfTokens<H, A> = SpanTokenOf<Tokens<H, A>, A>;
type ViewOfSlots<H, A> = ptr::NonNull<Tokens<H, A>>;
type QueueView<'a, H, A> = EntryView<'a, TokenQueue<H, A>>;
pub struct TokenQueue<H: Envelope, A: MemAllocator> {
    header: Header,
    buf: TokenOfTokens<H, A>,
}
unsafe impl<H: Send + Envelope, A: MemAllocator> Send for TokenQueue<H, A> {}
unsafe impl<H: Send + Envelope, A: MemAllocator> Sync for TokenQueue<H, A> {}
impl<H: Envelope, A: MemAllocator> UnwindSafe for TokenQueue<H, A> {}
impl<H: Envelope, A: MemAllocator> RefUnwindSafe for TokenQueue<H, A> {}

impl<H: Envelope, A: MemAllocator> Resource for TokenQueue<H, A> {
    type Config = usize;
    type Ctx = A;
    fn new(cfg: Self::Config, ctx: Self::Ctx) -> (Self, Self::Ctx) {
        let cap = cfg;
        let alloc = ctx;
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
        (TokenQueue { header: h, buf }, alloc)
    }

    fn free(s: Self, ctx: Self::Ctx) -> Self::Ctx {
        let alloc = ctx;
        // let (view, alloc) = self.project(alloc);
        // struct DropView<'a, H: Envelope, A: MemAllocator> {
        //     h: &'a Header,
        //     view: ptr::NonNull<Tokens<H, A>>,
        // }

        // impl<H: Envelope, A: MemAllocator> Queue for DropView<'_, H, A> {
        //     type Item = SpanPackToken<H, A>;

        //     fn header(&self) -> &Header {
        //         self.h
        //     }

        //     fn buf(&self) -> &Slots<Self::Item> {
        //         unsafe { self.view.as_ref() }
        //     }
        // }

        // let drop_view = DropView::<'_, _, A> { h: &self.h, view };
        // unsafe { drop_view.drop_in() };
        let Self { header: _, buf } = s;
        let b = buf.detoken(alloc);
        PBox::drop_in(b)
    }
}

impl<H: Envelope, A: MemAllocator> Project for TokenQueue<H, A> {
    type View = ViewOfSlots<H, A>;

    #[inline]
    fn project(&self, ctx: Self::Ctx) -> (Self::View, Self::Ctx) {
        let alloc = ctx;
        let (buf, alloc) = self.buf.as_ptr(alloc);
        (buf, alloc)
    }
}

impl<H: Envelope, A: MemAllocator> Queue for QueueView<'_, H, A> {
    type Item = SpanPackToken<H, A>;

    #[inline]
    fn header(&self) -> &Header {
        &self.guard.as_ref().header
    }

    #[inline]
    fn buf(&self) -> &Slots<Self::Item> {
        unsafe { self.view.as_ref() }
    }
}

impl<'a, H: Envelope, A: MemAllocator> Channel for QueueView<'a, H, A> {}

type ChannelView<'a, H, A> = EntryView<'a, TokenChannel<H, A>>;
pub struct TokenChannel<H: Envelope, A: MemAllocator> {
    l: TokenQueue<H, A>,
    r: TokenQueue<H, A>,
}

#[repr(transparent)]
#[derive(Clone)]
struct LQueue<T>(T);

#[repr(transparent)]
#[derive(Clone)]
struct RQueue<T>(T);

impl<H: Envelope, A: MemAllocator> Resource for TokenChannel<H, A> {
    type Config = usize;
    type Ctx = A;

    fn new(cfg: Self::Config, ctx: Self::Ctx) -> (Self, Self::Ctx) {
        let alloc = ctx;
        let (l, alloc) = TokenQueue::new(cfg, alloc);
        let (r, alloc) = TokenQueue::new(cfg, alloc);
        (Self { l, r }, alloc)
    }

    fn free(s: Self, ctx: Self::Ctx) -> Self::Ctx {
        let alloc = ctx;
        let Self { l, r } = s;
        let alloc = TokenQueue::free(l, alloc);
        TokenQueue::free(r, alloc)
    }
}

impl<H: Envelope, A: MemAllocator> Project for TokenChannel<H, A> {
    type View = (ViewOfSlots<H, A>, ViewOfSlots<H, A>);

    fn project(&self, ctx: Self::Ctx) -> (Self::View, Self::Ctx) {
        let alloc = ctx;
        let (l, alloc) = self.l.project(alloc);
        let (r, alloc) = self.r.project(alloc);
        ((l, r), alloc)
    }
}

impl<'a, H: Envelope, A: MemAllocator> Queue for LQueue<ChannelView<'a, H, A>> {
    type Item = SpanPackToken<H, A>;

    fn header(&self) -> &Header {
        &self.0.guard.as_ref().l.header
    }

    fn buf(&self) -> &Slots<Self::Item> {
        unsafe { self.0.view.0.as_ref() }
    }
}

impl<'a, H: Envelope, A: MemAllocator> Channel for LQueue<ChannelView<'a, H, A>> {}

impl<'a, H: Envelope, A: MemAllocator> Queue for RQueue<ChannelView<'a, H, A>> {
    type Item = SpanPackToken<H, A>;

    fn header(&self) -> &Header {
        &self.0.guard.as_ref().r.header
    }

    fn buf(&self) -> &Slots<Self::Item> {
        unsafe { self.0.view.1.as_ref() }
    }
}

impl<'a, H: Envelope, A: MemAllocator> Channel for RQueue<ChannelView<'a, H, A>> {}

impl<'a, H: Envelope, A: MemAllocator> ChannelView<'a, H, A> {
    fn lchannel(self) -> Duplex<LQueue<Self>, RQueue<Self>> {
        let lq = LQueue(self.clone());
        let rq = RQueue(self.clone());
        Duplex {
            s: lq.sender(),
            r: rq.receiver(),
        }
    }

    fn rchannel(self) -> Duplex<RQueue<Self>, LQueue<Self>> {
        let lq = LQueue(self.clone());
        let rq = RQueue(self.clone());
        Duplex {
            s: rq.sender(),
            r: lq.receiver(),
        }
    }
}

pub trait Channel: Clone {
    #[inline(always)]
    fn channel(self) -> (Sender<Self>, Receiver<Self>) {
        (Sender(self.clone()), Receiver(self))
    }

    #[inline(always)]
    fn sender(self) -> Sender<Self> {
        Sender(self)
    }

    #[inline(always)]
    fn receiver(self) -> Receiver<Self> {
        Receiver(self)
    }
}

#[repr(transparent)]
#[derive(Clone)]
struct Sender<S>(S);

#[repr(transparent)]
#[derive(Clone)]
struct Receiver<R>(R);

impl<S: Queue> Sender<S> {
    #[inline(always)]
    fn try_send(&self, value: S::Item) -> Result<(), S::Item> {
        self.0.push(value)
    }

    #[inline(always)]
    fn capacity(&self) -> usize {
        self.0.capacity()
    }

    #[inline(always)]
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[inline(always)]
    fn is_full(&self) -> bool {
        self.0.is_full()
    }

    #[inline(always)]
    fn len(&self) -> usize {
        self.0.len()
    }
}

impl<S: Queue> Receiver<S> {
    #[inline(always)]
    fn try_recv(&self) -> Option<S::Item> {
        self.0.pop()
    }

    #[inline(always)]
    fn capacity(&self) -> usize {
        self.0.capacity()
    }

    #[inline(always)]
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[inline(always)]
    fn is_full(&self) -> bool {
        self.0.is_full()
    }

    #[inline(always)]
    fn len(&self) -> usize {
        self.0.len()
    }
}

#[derive(Clone)]
struct Duplex<S, R> {
    s: Sender<S>,
    r: Receiver<R>,
}

impl<S: Queue, R: Queue> Duplex<S, R> {
    #[inline(always)]
    fn try_send(&self, value: S::Item) -> Result<(), S::Item> {
        self.s.try_send(value)
    }

    #[inline(always)]
    fn try_recv(&self) -> Option<R::Item> {
        self.r.try_recv()
    }
}

mod local {
    use core::{
        cell::UnsafeCell,
        mem::MaybeUninit,
        panic::{RefUnwindSafe, UnwindSafe},
        sync::atomic::AtomicUsize,
    };

    use crate::queue::{Header, Slot, Slots};

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
            use crate::queue::QueueDrop;
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
}
