use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{self, AtomicU8, AtomicU16, AtomicUsize, Ordering};

use crossbeam_utils::CachePadded;

pub mod cross;
pub mod driver;

/// One shared queue position. Control owns bytes before either cursor moves.
#[repr(C)]
pub struct Slot<T> {
    turn: AtomicUsize,
    control: AtomicU16,
    value: UnsafeCell<MaybeUninit<T>>,
}

const EMPTY: u16 = 0;
const PRODUCER: u16 = 1;
const AVAILABLE: u16 = 2;
const CONSUMER: u16 = 3;
const BASE_MASK: u16 = 0b11;
const COMPLETE: u16 = 1 << 2;
const SOURCE_SHIFT: u32 = 3;
const SOURCE_MASK: u16 = 0x3f << SOURCE_SHIFT;
const REAPER_SHIFT: u32 = 9;
const REAPER_MASK: u16 = 0x3f << REAPER_SHIFT;
const RESERVED_MASK: u16 = 1 << 15;

#[cfg(test)]
static EXIT_AFTER_REAPER: AtomicU8 = AtomicU8::new(0);

#[cfg(test)]
pub(crate) fn exit_after_reaper(owner: u8) {
    EXIT_AFTER_REAPER.store(owner + 1, Ordering::Relaxed);
}

#[cfg(test)]
pub(crate) const SEND_CLOSING: usize = 1;
#[cfg(test)]
pub(crate) const SEND_CLOSED: usize = 2;
#[cfg(test)]
pub(crate) const SEND_FINISHED: usize = 3;
#[cfg(test)]
pub(crate) const RECV_CLOSING: usize = 4;
#[cfg(test)]
pub(crate) const RECV_CLOSED: usize = 5;
#[cfg(test)]
pub(crate) const RECV_FINISHED: usize = 6;
#[cfg(test)]
static CLOSE_CRASH: AtomicU8 = AtomicU8::new(0);

#[cfg(test)]
pub(crate) fn crash_close_for_test(point: usize) {
    CLOSE_CRASH.store(point as u8, Ordering::Relaxed);
}

#[cfg(test)]
fn crash_close(point: usize) {
    if CLOSE_CRASH.load(Ordering::Relaxed) == point as u8 {
        std::process::exit(140 + point as i32);
    }
}

const _: () = {
    assert!(crate::header::PARTICIPANT_CAPACITY <= 63);
    assert!(BASE_MASK & (COMPLETE | SOURCE_MASK | REAPER_MASK | RESERVED_MASK) == 0);
    assert!(COMPLETE & (SOURCE_MASK | REAPER_MASK | RESERVED_MASK) == 0);
    assert!(SOURCE_MASK & (REAPER_MASK | RESERVED_MASK) == 0);
    assert!(REAPER_MASK & RESERVED_MASK == 0);
};

impl<T> Slot<T> {
    const fn new(turn: usize) -> Self {
        Self {
            turn: AtomicUsize::new(turn),
            control: AtomicU16::new(EMPTY),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
}

const fn owned(base: u16, complete: bool, owner: u8) -> u16 {
    base | if complete { COMPLETE } else { 0 } | (((owner as u16) + 1) << SOURCE_SHIFT)
}

const fn base(control: u16) -> u16 {
    control & BASE_MASK
}

const fn canonical(control: u16) -> bool {
    if control & RESERVED_MASK != 0 {
        return false;
    }
    let source = (control & SOURCE_MASK) >> SOURCE_SHIFT;
    let reaper = (control & REAPER_MASK) >> REAPER_SHIFT;
    match base(control) {
        EMPTY => control == EMPTY,
        PRODUCER | CONSUMER => {
            source != 0
                && source as usize <= crate::header::PARTICIPANT_CAPACITY
                && reaper as usize <= crate::header::PARTICIPANT_CAPACITY
        }
        AVAILABLE => source == 0 && reaper == 0,
        _ => false,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReserveError {
    Full,
    Busy,
    Contended,
    Closed,
}

pub enum ClaimError {
    Empty,
    Busy,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Repair {
    None,
    Recovered,
    Busy(u8),
    Corrupted,
}

#[must_use]
pub struct Reserved<'a, Q: Queue + ?Sized> {
    slot: &'a Slot<Q::Item>,
    done: bool,
}

#[must_use]
pub struct Staged<'a, Q: Queue + ?Sized> {
    reserved: Reserved<'a, Q>,
}

impl<'a, Q: Queue + ?Sized> Reserved<'a, Q> {
    pub fn stage(self, value: Q::Item) -> Staged<'a, Q> {
        unsafe { (*self.slot.value.get()).write(value) };
        self.slot.control.store(
            owned(PRODUCER, true, self.reserved_owner()),
            Ordering::Release,
        );
        Staged { reserved: self }
    }

    fn reserved_owner(&self) -> u8 {
        let source = (self.slot.control.load(Ordering::Relaxed) & SOURCE_MASK) >> SOURCE_SHIFT;
        debug_assert_ne!(source, 0);
        (source - 1) as u8
    }

    fn skip(&mut self) {
        self.slot.control.store(AVAILABLE, Ordering::Release);
        self.done = true;
    }
}

impl<Q: Queue + ?Sized> Drop for Reserved<'_, Q> {
    fn drop(&mut self) {
        if !self.done {
            self.skip();
        }
    }
}

impl<Q: Queue + ?Sized> Staged<'_, Q> {
    pub fn publish(mut self) {
        self.reserved
            .slot
            .control
            .store(AVAILABLE | COMPLETE, Ordering::Release);
        self.reserved.done = true;
    }

    #[cfg(test)]
    pub fn cancel(mut self) -> Q::Item {
        let value = unsafe { self.reserved.slot.value.get().read().assume_init() };
        self.reserved.skip();
        value
    }
}

impl<Q: Queue + ?Sized> Drop for Staged<'_, Q> {
    fn drop(&mut self) {
        if !self.reserved.done {
            unsafe { (*self.reserved.slot.value.get()).assume_init_drop() };
            self.reserved.skip();
        }
    }
}

#[must_use]
pub struct Claim<'a, Q: Queue + ?Sized> {
    slot: &'a Slot<Q::Item>,
    next_turn: usize,
    initialized: bool,
    done: bool,
}

impl<Q: Queue + ?Sized> Claim<'_, Q> {
    fn recycle(&mut self) {
        self.slot.turn.store(self.next_turn, Ordering::Release);
        self.slot.control.store(EMPTY, Ordering::Release);
        self.done = true;
    }

    pub fn take(mut self) -> Q::Item {
        debug_assert!(self.initialized);
        let value = unsafe { self.slot.value.get().read().assume_init() };
        self.initialized = false;
        self.recycle();
        value
    }
}

impl<Q: Queue + ?Sized> Drop for Claim<'_, Q> {
    fn drop(&mut self) {
        if !self.done {
            if self.initialized {
                unsafe { (*self.slot.value.get()).assume_init_drop() };
            }
            self.recycle();
        }
    }
}

pub struct Header {
    head: CachePadded<AtomicUsize>,
    tail: CachePadded<AtomicUsize>,
}

impl Header {
    pub(crate) const fn new() -> Self {
        Self {
            head: CachePadded::new(AtomicUsize::new(0)),
            tail: CachePadded::new(AtomicUsize::new(0)),
        }
    }
}

pub trait Queue {
    type Item;

    fn header(&self) -> &Header;
    fn buf(&self) -> &[Slot<Self::Item>];
    fn lifecycle(&self) -> &AtomicU8;
    fn send_field(&self) -> u32;
    fn recv_field(&self) -> u32;
    fn owner(&self) -> u8;
    fn one_lap(&self) -> usize;
}

pub trait QueueOps: Queue {
    fn reserve(&self) -> Result<Reserved<'_, Self>, ReserveError>
    where
        Self: Sized,
    {
        let header = self.header();
        if self.state() != 0 {
            return Err(ReserveError::Closed);
        }
        let tail = header.tail.load(Ordering::Relaxed);
        let one_lap = self.one_lap();
        let index = tail & (one_lap - 1);
        let lap = tail & !(one_lap - 1);
        let new_tail = if index + 1 < self.capacity() {
            tail + 1
        } else {
            lap.wrapping_add(one_lap)
        };
        let slot = &self.buf()[index];
        if slot
            .control
            .compare_exchange(
                EMPTY,
                owned(PRODUCER, false, self.owner()),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            atomic::fence(Ordering::SeqCst);
            return if header.head.load(Ordering::Relaxed).wrapping_add(one_lap) == tail {
                Err(ReserveError::Full)
            } else {
                Err(ReserveError::Busy)
            };
        }
        if slot.turn.load(Ordering::Acquire) != tail || header.tail.load(Ordering::Relaxed) != tail
        {
            slot.control.store(EMPTY, Ordering::Release);
            return Err(ReserveError::Busy);
        }
        if self.state() != 0 {
            slot.control.store(EMPTY, Ordering::Release);
            return Err(ReserveError::Closed);
        }
        if header
            .tail
            .compare_exchange(tail, new_tail, Ordering::SeqCst, Ordering::Relaxed)
            .is_err()
        {
            slot.control.store(EMPTY, Ordering::Release);
            return Err(ReserveError::Contended);
        }
        Ok(Reserved { slot, done: false })
    }

    fn claim(&self) -> Result<Claim<'_, Self>, ClaimError>
    where
        Self: Sized,
    {
        if self.field_state(self.recv_field()) != 0 {
            return Err(ClaimError::Closed);
        }
        let header = self.header();
        let head = header.head.load(Ordering::Relaxed);
        let buf = self.buf();
        let one_lap = self.one_lap();
        let index = head & (one_lap - 1);
        let lap = head & !(one_lap - 1);
        let slot = &buf[index];
        let new = if index + 1 < self.capacity() {
            head + 1
        } else {
            lap.wrapping_add(one_lap)
        };
        let available = slot.control.load(Ordering::Acquire);
        if available != AVAILABLE && available != (AVAILABLE | COMPLETE) {
            atomic::fence(Ordering::SeqCst);
            return if header.tail.load(Ordering::Relaxed) == head {
                Err(ClaimError::Empty)
            } else {
                Err(ClaimError::Busy)
            };
        }
        let initialized = available & COMPLETE != 0;
        if slot
            .control
            .compare_exchange(
                available,
                owned(CONSUMER, initialized, self.owner()),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return Err(ClaimError::Busy);
        }
        if slot.turn.load(Ordering::Acquire) != head || header.head.load(Ordering::Relaxed) != head
        {
            slot.control.store(available, Ordering::Release);
            return Err(ClaimError::Busy);
        }
        if header
            .head
            .compare_exchange(head, new, Ordering::SeqCst, Ordering::Relaxed)
            .is_err()
        {
            slot.control.store(available, Ordering::Release);
            return Err(ClaimError::Busy);
        }
        let mut claim = Claim {
            slot,
            next_turn: head.wrapping_add(one_lap),
            initialized,
            done: false,
        };
        if !initialized {
            claim.recycle();
            return Err(ClaimError::Busy);
        }
        Ok(claim)
    }

    fn repair(&self, index: usize, dead: u8, live: u8) -> Repair {
        if dead as usize >= crate::header::PARTICIPANT_CAPACITY
            || live as usize >= crate::header::PARTICIPANT_CAPACITY
        {
            return Repair::Corrupted;
        }
        let Some(slot) = self.buf().get(index) else {
            return Repair::Corrupted;
        };
        let control = slot.control.load(Ordering::Acquire);
        if !canonical(control) {
            return Repair::Corrupted;
        }
        let source = ((control & SOURCE_MASK) >> SOURCE_SHIFT) as u8;
        let reaper = ((control & REAPER_MASK) >> REAPER_SHIFT) as u8;
        if !matches!(base(control), PRODUCER | CONSUMER) {
            return Repair::None;
        }
        let next = if source == dead + 1 && reaper == live + 1 {
            control
        } else if reaper == dead + 1 {
            (control & !REAPER_MASK) | (((live as u16) + 1) << REAPER_SHIFT)
        } else if source == dead + 1 && reaper == 0 {
            control | (((live as u16) + 1) << REAPER_SHIFT)
        } else if source == dead + 1 {
            return Repair::Busy(reaper - 1);
        } else {
            return Repair::None;
        };
        if next != control
            && slot
                .control
                .compare_exchange(control, next, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return Repair::Busy(live);
        }
        #[cfg(test)]
        if next != control && EXIT_AFTER_REAPER.load(Ordering::Relaxed) == live + 1 {
            std::process::exit(74);
        }

        let complete = control & COMPLETE;
        match base(control) {
            PRODUCER => {
                let turn = slot.turn.load(Ordering::Acquire);
                let tail = self.header().tail.load(Ordering::Acquire);
                let distance = tail.wrapping_sub(turn);
                if distance == 0 {
                    if complete != 0 {
                        return Repair::Corrupted;
                    }
                    slot.control.store(EMPTY, Ordering::Release);
                } else if distance < 1usize << (usize::BITS - 1) {
                    slot.control.store(AVAILABLE | complete, Ordering::Release);
                } else {
                    return Repair::Corrupted;
                }
            }
            CONSUMER => {
                let turn = slot.turn.load(Ordering::Acquire);
                let head = self.header().head.load(Ordering::Acquire);
                if head == turn {
                    slot.control.store(AVAILABLE | complete, Ordering::Release);
                } else {
                    let distance = head.wrapping_sub(turn);
                    if distance != 0 && distance < 1usize << (usize::BITS - 1) {
                        slot.turn
                            .store(turn.wrapping_add(self.one_lap()), Ordering::Release);
                    }
                    slot.control.store(EMPTY, Ordering::Release);
                }
            }
            _ => unreachable!(),
        }
        Repair::Recovered
    }

    #[inline]
    fn capacity(&self) -> usize {
        self.buf().len()
    }

    fn is_empty(&self) -> bool {
        let header = self.header();
        let head = header.head.load(Ordering::SeqCst);
        let tail = header.tail.load(Ordering::SeqCst);
        tail == head
    }

    fn is_full(&self) -> bool {
        let header = self.header();
        let tail = header.tail.load(Ordering::SeqCst);
        let head = header.head.load(Ordering::SeqCst);
        head.wrapping_add(self.one_lap()) == tail
    }

    fn len(&self) -> usize {
        let header = self.header();
        let tail = header.tail.load(Ordering::SeqCst);
        let head = header.head.load(Ordering::SeqCst);
        let one_lap = self.one_lap();
        let hix = head & (one_lap - 1);
        let tix = tail & (one_lap - 1);
        if hix < tix {
            tix - hix
        } else if hix > tix {
            self.capacity() - hix + tix
        } else if tail == head {
            0
        } else {
            self.capacity()
        }
    }
    fn field_state(&self, field: u32) -> u8 {
        (self.lifecycle().load(Ordering::Acquire) >> field) & 0b11
    }

    fn state(&self) -> u8 {
        self.field_state(self.send_field()) | self.field_state(self.recv_field())
    }

    fn close_field(&self, field: u32) {
        self.lifecycle().fetch_or(1 << field, Ordering::AcqRel);
    }

    fn finish_field(&self, field: u32, owned_base: u16) -> u8 {
        let state = self.field_state(field);
        if state == 1
            && self
                .buf()
                .iter()
                .all(|slot| base(slot.control.load(Ordering::Acquire)) != owned_base)
        {
            self.lifecycle().fetch_or(2 << field, Ordering::AcqRel);
            #[cfg(test)]
            crash_close(if owned_base == PRODUCER {
                SEND_FINISHED
            } else {
                RECV_FINISHED
            });
            3
        } else {
            state
        }
    }

    fn close_send(&self) {
        #[cfg(test)]
        crash_close(SEND_CLOSING);
        self.close_field(self.send_field());
        #[cfg(test)]
        crash_close(SEND_CLOSED);
    }

    fn close_recv(&self) {
        #[cfg(test)]
        crash_close(RECV_CLOSING);
        self.close_field(self.recv_field());
        #[cfg(test)]
        crash_close(RECV_CLOSED);
    }

    fn send_closed(&self) -> bool {
        self.finish_field(self.send_field(), PRODUCER) == 3
    }

    fn recv_closed(&self) -> bool {
        self.finish_field(self.recv_field(), CONSUMER) == 3
    }

    fn terminal(&self) -> bool {
        self.recv_closed() || (self.send_closed() && self.is_empty())
    }
}

impl<T: Queue> QueueOps for T {}

pub trait Sender {
    type Item;
    type TryError;

    fn try_send(&self, item: Self::Item) -> Result<(), Self::TryError>;
}

pub trait Receiver {
    type Item;
    type TryError;

    fn try_recv(&self) -> Result<Self::Item, Self::TryError>;
}

pub trait QueueChannel {
    type Handle: Queue;

    fn handle(&self) -> &Self::Handle;
    fn close(&self);
    fn is_close(&self) -> bool;

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
    Disconnected(T),
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

    fn close(&self) {
        self.tx.close_send()
    }

    fn is_close(&self) -> bool {
        self.tx.field_state(self.tx.send_field()) != 0
    }
}

impl<T: Queue> QueueTx<T> {
    #[inline(always)]
    pub fn try_send(&self, value: T::Item) -> Result<(), TrySendError<T::Item>> {
        match self.tx.reserve() {
            Ok(reserved) => {
                reserved.stage(value).publish();
                Ok(())
            }
            Err(ReserveError::Closed) => Err(TrySendError::Disconnected(value)),
            Err(ReserveError::Full | ReserveError::Busy | ReserveError::Contended) => {
                Err(TrySendError::Full(value))
            }
        }
    }
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

    fn close(&self) {
        self.rx.close_recv()
    }

    fn is_close(&self) -> bool {
        self.rx.field_state(self.rx.recv_field()) != 0
    }
}

impl<T: Queue> QueueRx<T> {
    #[inline(always)]
    pub fn try_recv(&self) -> Result<T::Item, TryRecvError> {
        match self.rx.claim() {
            Ok(claim) => Ok(claim.take()),
            Err(ClaimError::Closed) => Err(TryRecvError::Disconnected),
            Err(ClaimError::Empty) if self.rx.terminal() => Err(TryRecvError::Disconnected),
            Err(ClaimError::Empty | ClaimError::Busy) => Err(TryRecvError::Empty),
        }
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::{
        AVAILABLE, BASE_MASK, COMPLETE, CONSUMER, EMPTY, Header, PRODUCER, Queue, QueueOps,
        QueueRx, QueueTx, REAPER_MASK, RESERVED_MASK, SOURCE_MASK, Slot, TryRecvError,
        TrySendError, canonical,
    };
    use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

    struct TestQueue {
        header: Header,
        slots: Box<[Slot<u64>]>,
        lifecycle: AtomicU8,
        one_lap: usize,
    }

    unsafe impl Sync for TestQueue {}

    impl TestQueue {
        fn new(capacity: usize) -> Self {
            Self {
                header: Header::new(),
                slots: (0..capacity).map(Slot::new).collect(),
                lifecycle: AtomicU8::new(0),
                one_lap: (capacity + 1).next_power_of_two(),
            }
        }
    }

    impl Queue for &TestQueue {
        type Item = u64;
        fn header(&self) -> &Header {
            &self.header
        }
        fn buf(&self) -> &[Slot<Self::Item>] {
            &self.slots
        }
        fn lifecycle(&self) -> &AtomicU8 {
            &self.lifecycle
        }
        fn send_field(&self) -> u32 {
            0
        }
        fn recv_field(&self) -> u32 {
            2
        }
        fn owner(&self) -> u8 {
            0
        }
        fn one_lap(&self) -> usize {
            self.one_lap
        }
    }

    #[test]
    fn disconnected_send_returns_the_exact_item() {
        let queue = TestQueue::new(1);
        (&queue).close_send();
        let sender = QueueTx { tx: &queue };
        match sender.try_send(41) {
            Err(TrySendError::Disconnected(value)) => assert_eq!(value, 41),
            _ => panic!("closed queue must return the submitted item"),
        }
        let receiver = QueueRx { rx: &queue };
        assert!(matches!(
            receiver.try_recv(),
            Err(super::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn close_becomes_terminal_only_after_drain() {
        let empty = TestQueue::new(1);
        (&empty).close_send();
        assert!((&empty).terminal());

        let nonempty = TestQueue::new(1);
        QueueTx { tx: &nonempty }.try_send(7).unwrap();
        (&nonempty).close_send();
        assert!(!(&nonempty).terminal());
        assert_eq!(QueueRx { rx: &nonempty }.try_recv().unwrap(), 7);
        assert!((&nonempty).terminal());
    }

    #[test]
    fn cancelled_reservations_commit_ordered_skips() {
        let queue = TestQueue::new(2);
        let queue_ref = &queue;
        drop(queue_ref.reserve().expect("reserve first turn"));
        let staged = queue_ref.reserve().expect("reserve second turn").stage(17);
        assert_eq!(staged.cancel(), 17);

        let receiver = QueueRx { rx: &queue };
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
        QueueTx { tx: &queue }.try_send(23).unwrap();
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
        assert_eq!(receiver.try_recv().unwrap(), 23);
    }

    #[test]
    fn close_accounts_for_every_racing_sender() {
        use std::sync::Barrier;

        const SENDERS: usize = 64;
        let queue = TestQueue::new(128);
        let barrier = Barrier::new(SENDERS + 2);
        let accepted = AtomicUsize::new(0);
        let rejected = AtomicUsize::new(0);

        std::thread::scope(|scope| {
            for value in 0..SENDERS as u64 {
                let barrier = &barrier;
                let queue = &queue;
                let accepted = &accepted;
                let rejected = &rejected;
                scope.spawn(move || {
                    barrier.wait();
                    match (QueueTx { tx: queue }).try_send(value) {
                        Ok(()) => {
                            accepted.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(TrySendError::Disconnected(returned)) => {
                            assert_eq!(returned, value);
                            rejected.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(TrySendError::Full(returned)) => {
                            assert_eq!(returned, value);
                            rejected.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                });
            }
            scope.spawn(|| {
                barrier.wait();
                (&queue).close_send();
            });
            barrier.wait();
        });

        let receiver = QueueRx { rx: &queue };
        let mut received = 0;
        loop {
            match receiver.try_recv() {
                Ok(_) => received += 1,
                Err(super::TryRecvError::Disconnected) => break,
                Err(super::TryRecvError::Empty) => std::thread::yield_now(),
            }
        }
        assert_eq!(received, accepted.load(Ordering::Relaxed));
        assert_eq!(received + rejected.load(Ordering::Relaxed), SENDERS);
    }

    #[test]
    fn only_defined_control_encodings_are_canonical() {
        for control in u16::MIN..=u16::MAX {
            let source = control & SOURCE_MASK;
            let reaper = control & REAPER_MASK;
            let expected = control & RESERVED_MASK == 0
                && match control & BASE_MASK {
                    EMPTY => control == EMPTY,
                    AVAILABLE => source == 0 && reaper == 0,
                    PRODUCER | CONSUMER => {
                        source != 0
                            && (source >> super::SOURCE_SHIFT) as usize
                                <= crate::header::PARTICIPANT_CAPACITY
                            && (reaper >> super::REAPER_SHIFT) as usize
                                <= crate::header::PARTICIPANT_CAPACITY
                    }
                    _ => false,
                };
            assert_eq!(canonical(control), expected, "{control:#018b}");
        }
        assert!(canonical(AVAILABLE | COMPLETE));
    }

    #[test]
    fn dead_reaper_is_replaced_without_losing_the_source_phase() {
        let queue = TestQueue::new(1);
        queue.slots[0].control.store(
            super::owned(PRODUCER, false, 0) | (2 << super::REAPER_SHIFT),
            Ordering::Release,
        );
        queue.header.tail.store(1, Ordering::Release);

        assert_eq!((&queue).repair(0, 1, 2), super::Repair::Recovered);
        assert_eq!(queue.slots[0].control.load(Ordering::Acquire), AVAILABLE);
    }

    #[test]
    fn live_reaper_resumes_its_persisted_claim() {
        let queue = TestQueue::new(1);
        queue.slots[0].control.store(
            super::owned(PRODUCER, false, 0) | (2 << super::REAPER_SHIFT),
            Ordering::Release,
        );

        assert_eq!((&queue).repair(0, 0, 1), super::Repair::Recovered);
        assert_eq!(queue.slots[0].control.load(Ordering::Acquire), EMPTY);
    }

    #[test]
    fn repair_uses_cursor_commit_and_never_guesses_from_bytes() {
        let before_send = TestQueue::new(1);
        before_send.slots[0]
            .control
            .store(super::owned(PRODUCER, false, 0), Ordering::Release);
        assert_eq!((&before_send).repair(0, 0, 1), super::Repair::Recovered);
        assert_eq!(before_send.slots[0].control.load(Ordering::Acquire), EMPTY);

        let sent = TestQueue::new(1);
        unsafe { (*sent.slots[0].value.get()).write(37) };
        sent.slots[0]
            .control
            .store(super::owned(PRODUCER, true, 0), Ordering::Release);
        sent.header.tail.store(1, Ordering::Release);
        assert_eq!((&sent).repair(0, 0, 1), super::Repair::Recovered);
        assert_eq!(QueueRx { rx: &sent }.try_recv().unwrap(), 37);

        let taking = TestQueue::new(1);
        taking.slots[0]
            .control
            .store(super::owned(CONSUMER, true, 0), Ordering::Release);
        taking.header.head.store(1, Ordering::Release);
        assert_eq!((&taking).repair(0, 0, 1), super::Repair::Recovered);
        assert_eq!(taking.slots[0].turn.load(Ordering::Acquire), 2);
        assert_eq!(taking.slots[0].control.load(Ordering::Acquire), EMPTY);
    }
}
