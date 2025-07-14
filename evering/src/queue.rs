use core::{
    fmt,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
};

pub(crate) struct Queue<'a, T> {
    pub off: &'a Range,
    pub buf: NonNull<T>,
}

impl<'a, T> Queue<'a, T> {
    pub fn len(&self) -> usize {
        self.off.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn enqueue(&mut self, val: T) -> Result<(), T> {
        let Self { off, buf } = self;
        debug_assert!((off.size()).is_power_of_two());

        let write_idx = match off.reserve_tail() {
            Ok(tail) => tail,
            Err(_) => return Err(val),
        };

        unsafe { buf.add(write_idx).write(val) };
        Ok(())
    }

    pub fn enqueue_bulk(&mut self, vals: impl Iterator<Item = T>) -> usize {
        let mut vals = vals;

        let Self { off, buf } = self;
        debug_assert!((off.size()).is_power_of_two());

        let mut suc = 0;
        let suc = loop {
            let write_idx = match off.reserve_tail() {
                Ok(idx) => idx,
                Err(_) => {
                    break suc;
                }
            };

            let Some(val) = vals.next() else {
                break suc;
            };
            unsafe {
                buf.add(write_idx).write(val);
            }

            suc += 1;
        };

        suc
    }

    pub fn dequeue(&mut self) -> Option<T> {
        let Self { off, buf } = self;
        debug_assert!((off.size()).is_power_of_two());

        let read_idx = match off.reserve_head() {
            Ok(idx) => idx,
            Err(_) => return None,
        };

        let val = unsafe { buf.add(read_idx).read() };
        Some(val)
    }

    pub fn dequeue_bulk(&mut self) -> Drain<'a, T> {
        let Self { off, buf } = self;
        debug_assert!((off.size()).is_power_of_two());

        let head = off.head.load(Ordering::Relaxed);
        let tail = off.tail.load(Ordering::Acquire);

        Drain {
            off,
            buf: *buf,
            head,
            tail,
        }
    }

    pub fn drop_elems(&mut self) {
        let Self { off, buf } = self;
        debug_assert!((off.size()).is_power_of_two());

        loop {
            let read_idx = match off.reserve_head() {
                Ok(idx) => idx,
                Err(RangeError::Empty) => {
                    break;
                }
                Err(_) => unreachable!(
                    "[Queue]: try to reserve tail should only return Ok(idx) or Err(Empty)"
                ),
            };

            unsafe {
                buf.add(read_idx).drop_in_place();
            }
        }
    }
}

pub struct Drain<'a, T> {
    off: &'a Range,
    buf: NonNull<T>,
    head: usize,
    tail: usize,
}

impl<T> Iterator for Drain<'_, T> {
    type Item = T;
    fn next(&mut self) -> Option<Self::Item> {
        if self.head == self.tail {
            return None;
        }
        let next_head = self.off.inc(self.head);
        let val = unsafe { self.buf.add(self.head as usize).read() };
        self.off.head.store(next_head, Ordering::Release);
        self.head = next_head;
        Some(val)
    }
}

pub(crate) struct Range {
    head: AtomicUsize,
    tail: AtomicUsize,
    pub mask: usize,
}

#[derive(Debug)]
pub(crate) enum RangeError {
    Full,
    Empty,
    Contended,
}

impl fmt::Display for RangeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RangeError::Full => f.write_str("Full"),
            RangeError::Empty => f.write_str("Empty"),
            RangeError::Contended => f.write_str("Contended"),
        }
    }
}

impl core::error::Error for RangeError {}

pub type Head = usize;
pub type Tail = usize;

impl Range {
    pub fn new(size: Pow2) -> Self {
        // Safety: `Pow2` is always a power of 2 and greater than 1.
        let mask = size.as_usize() - 1;
        Self {
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            // TODO! handle underflow.
            mask,
        }
    }

    #[inline(always)]
    pub fn size(&self) -> usize {
        self.mask + 1
    }

    #[inline(always)]
    pub fn len(&self) -> usize {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Relaxed);
        tail.wrapping_sub(head) & self.mask
    }

    pub fn inc(&self, n: usize) -> usize {
        n.wrapping_add(1) & self.mask
    }

    pub fn reserve_tail(&self) -> Result<Tail, RangeError> {
        loop {
            let tail = self.tail.load(Ordering::Relaxed);
            let head = self.head.load(Ordering::Acquire);

            let next_tail = self.inc(tail);
            if next_tail == head {
                return Err(RangeError::Full);
            }

            match self.tail.compare_exchange_weak(
                tail,
                next_tail,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(tail),
                Err(_) => continue,
            }
        }
    }

    pub fn reserve_head(&self) -> Result<Head, RangeError> {
        loop {
            let head = self.head.load(Ordering::Relaxed);
            let tail = self.tail.load(Ordering::Acquire);

            let next_head = self.inc(head);
            if head == tail {
                return Err(RangeError::Empty);
            }

            match self.head.compare_exchange_weak(
                head,
                next_head,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(head),
                Err(_) => continue,
            }
        }
    }
}

#[derive(Clone, Copy)]
pub struct Pow2(usize);

impl Pow2 {
    pub const fn new(pow: usize) -> Self {
        // compile time check
        assert!(pow < usize::BITS as usize, "pow should be less than 64");
        Self(1 << pow)
    }

    pub const fn as_usize(self) -> usize {
        self.0
    }
}
