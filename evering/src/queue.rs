use core::{
    ptr::NonNull,
    sync::atomic::{AtomicU32, Ordering},
};

pub struct Queue<'a, T> {
    pub off: &'a Offsets,
    pub buf: NonNull<T>,
}

impl<'a, T> Queue<'a, T> {
    pub fn len(&self) -> usize {
        let head = self.off.head.load(Ordering::Relaxed);
        let tail = self.off.tail.load(Ordering::Relaxed);
        (tail.wrapping_sub(head) & self.off.ring_mask) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub unsafe fn enqueue(&mut self, val: T) -> Result<(), T> {
        let Self { off, buf } = self;
        debug_assert!((off.ring_mask + 1).is_power_of_two());

        let tail = off.tail.load(Ordering::Relaxed);
        let head = off.head.load(Ordering::Acquire);

        let next_tail = off.inc(tail);
        if next_tail == head {
            return Err(val);
        }

        unsafe { buf.add(tail as usize).write(val) };
        off.tail.store(next_tail, Ordering::Release);

        Ok(())
    }

    pub unsafe fn enqueue_bulk(&mut self, mut vals: impl Iterator<Item = T>) -> usize {
        let Self { off, buf } = self;
        debug_assert!((off.ring_mask + 1).is_power_of_two());

        let mut tail = off.tail.load(Ordering::Relaxed);
        let head = off.head.load(Ordering::Acquire);

        let mut n = 0;
        let mut next_tail;
        loop {
            next_tail = off.inc(tail);
            if next_tail == head {
                break;
            }
            let Some(val) = vals.next() else {
                break;
            };
            unsafe { buf.add(tail as usize).write(val) };
            off.tail.store(next_tail, Ordering::Release);
            n += 1;
            tail = next_tail;
        }

        n
    }

    pub unsafe fn dequeue(&mut self) -> Option<T> {
        let Self { off, buf } = self;
        debug_assert!((off.ring_mask + 1).is_power_of_two());

        let head = off.head.load(Ordering::Relaxed);
        let tail = off.tail.load(Ordering::Acquire);

        if head == tail {
            return None;
        }
        let next_head = off.inc(head);

        let val = unsafe { buf.add(head as usize).read() };
        off.head.store(next_head, Ordering::Release);

        Some(val)
    }

    pub unsafe fn dequeue_bulk(&mut self) -> Drain<'a, T> {
        let Self { off, buf } = self;
        debug_assert!((off.ring_mask + 1).is_power_of_two());

        let head = off.head.load(Ordering::Relaxed);
        let tail = off.tail.load(Ordering::Acquire);

        Drain {
            off,
            buf: *buf,
            head,
            tail,
        }
    }

    pub unsafe fn drop_in_place(&mut self) {
        debug_assert!((self.off.ring_mask + 1).is_power_of_two());
        unsafe {
            let mut head = self.off.head.as_ptr().read();
            let tail = self.off.tail.as_ptr().read();
            while head != tail {
                self.buf.add(head as usize).drop_in_place();
                head = self.off.inc(head);
            }
        }
    }
}

pub struct Drain<'a, T> {
    off: &'a Offsets,
    buf: NonNull<T>,
    head: u32,
    tail: u32,
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

pub struct Offsets {
    head: AtomicU32,
    tail: AtomicU32,
    pub ring_mask: u32,
}

impl Offsets {
    pub fn new(size: u32) -> Self {
        debug_assert!(size.is_power_of_two());
        Self {
            head: AtomicU32::new(0),
            tail: AtomicU32::new(0),
            ring_mask: size - 1,
        }
    }

    pub fn inc(&self, n: u32) -> u32 {
        n.wrapping_add(1) & self.ring_mask
    }
}
