use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{self, AtomicBool, AtomicUsize, Ordering};

use crossbeam_utils::{Backoff, CachePadded};

pub mod cross;
pub mod driver;
mod local;

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

    /// The disconnection.
    close: AtomicBool,
}

impl Header {
    const fn new(cap: usize) -> Self {
        assert!(cap > 0, "capacity must not zero");
        // Head is initialized to `{ lap: 0, index: 0 }`.
        // Tail is initialized to `{ lap: 0, index: 0 }`.
        let head = 0;
        let tail = 0;
        // One lap is the smallest power of two greater than `cap`.
        let one_lap = (cap + 1).next_power_of_two();
        Header {
            head: CachePadded::new(AtomicUsize::new(head)),
            tail: CachePadded::new(AtomicUsize::new(tail)),
            one_lap,
            cap,
            close: AtomicBool::new(false),
        }
    }

    pub fn close(&self) {
        self.close.store(true, Ordering::Release)
    }

    pub fn open(&self) {
        self.close.store(false, Ordering::Release)
    }

    pub fn is_close(&self) -> bool {
        self.close.load(Ordering::Relaxed)
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

trait QueueClose: Queue {
    #[inline]
    fn close(&self) {
        self.header().close()
    }

    #[inline]
    fn is_close(&self) -> bool {
        self.header().is_close()
    }
}

impl<T: Queue> QueueClose for T {}

trait Endpoint: Sized + Queue {
    #[inline(always)]
    fn sender(self) -> QueueTx<Self> {
        QueueTx { tx: self }
    }

    #[inline(always)]
    fn receiver(self) -> QueueRx<Self> {
        QueueRx { rx: self }
    }
}

pub trait Sender {
    type Item;
    type TryError;

    fn try_send(&self, item: Self::Item) -> Result<(), Self::TryError>;
}

pub trait AsyncSender: Sender {
    type Error;

    fn send(&self, item: Self::Item) -> impl Future<Output = Result<(), Self::Error>>;
}

pub trait Receiver {
    type Item;
    type TryError;

    fn try_recv(&self) -> Result<Self::Item, Self::TryError>;
}

pub trait AsyncReceiver: Receiver {
    type Error;

    fn recv(&self) -> impl Future<Output = Result<Self::Item, Self::Error>>;
}

pub trait QueueChannel {
    type Handle: Queue;

    fn handle(&self) -> &Self::Handle;

    #[inline(always)]
    fn close(&self) {
        self.handle().close()
    }

    #[inline(always)]
    fn is_close(&self) -> bool {
        self.handle().is_close()
    }

    #[inline(always)]
    fn capacity(&self) -> usize {
        self.handle().capacity()
    }

    #[inline(always)]
    fn is_empty(&self) -> bool {
        self.handle().is_empty()
    }

    #[inline(always)]
    fn is_full(&self) -> bool {
        self.handle().is_full()
    }

    #[inline(always)]
    fn len(&self) -> usize {
        self.handle().len()
    }
}

#[derive(Debug)]
pub enum TrySendError<T> {
    Full(T),
    Disconnected,
}

#[derive(Debug)]
pub enum TryRecvError {
    Empty,
    Disconnected,
}

#[derive(Clone, Debug, PartialEq)]
#[repr(transparent)]
pub struct QueueTx<T: Queue> {
    tx: T,
}

#[derive(Clone, Debug, PartialEq)]
#[repr(transparent)]
pub struct QueueRx<T: Queue> {
    rx: T,
}

impl<T: Queue> Sender for QueueTx<T> {
    type Item = T::Item;

    type TryError = TrySendError<T::Item>;

    #[inline(always)]
    fn try_send(&self, item: Self::Item) -> Result<(), Self::TryError> {
        self.try_send(item)
    }
}

impl<T: Queue> QueueChannel for QueueTx<T> {
    type Handle = T;

    #[inline(always)]
    fn handle(&self) -> &Self::Handle {
        &self.tx
    }
}

impl<T: Queue> QueueTx<T> {
    #[inline(always)]
    pub fn try_send(&self, value: T::Item) -> Result<(), TrySendError<T::Item>> {
        if self.tx.header().is_close() {
            return Err(TrySendError::Disconnected);
        }
        self.tx.push(value).map_err(TrySendError::Full)
    }

    //     #[inline(always)]
    //     pub fn capacity(&self) -> usize {
    //         self.tx.capacity()
    //     }

    //     #[inline(always)]
    //     pub fn is_empty(&self) -> bool {
    //         self.tx.is_empty()
    //     }

    //     #[inline(always)]
    //     pub fn is_full(&self) -> bool {
    //         self.tx.is_full()
    //     }

    //     #[inline(always)]
    //     pub fn len(&self) -> usize {
    //         self.tx.len()
    //     }
}

impl<T: Queue> Receiver for QueueRx<T> {
    type Item = T::Item;

    type TryError = TryRecvError;

    #[inline(always)]
    fn try_recv(&self) -> Result<Self::Item, Self::TryError> {
        self.try_recv()
    }
}

impl<T: Queue> QueueChannel for QueueRx<T> {
    type Handle = T;

    #[inline(always)]
    fn handle(&self) -> &Self::Handle {
        &self.rx
    }
}

impl<T: Queue> QueueRx<T> {
    #[inline(always)]
    pub fn try_recv(&self) -> Result<T::Item, TryRecvError> {
        self.rx.pop().ok_or_else(|| {
            if self.is_close() {
                TryRecvError::Disconnected
            } else {
                TryRecvError::Empty
            }
        })
    }

    // #[inline(always)]
    // pub fn capacity(&self) -> usize {
    //     self.rx.capacity()
    // }

    // #[inline(always)]
    // pub fn is_empty(&self) -> bool {
    //     self.rx.is_empty()
    // }

    // #[inline(always)]
    // pub fn is_full(&self) -> bool {
    //     self.rx.is_full()
    // }

    // #[inline(always)]
    // pub fn len(&self) -> usize {
    //     self.rx.len()
    // }
}
