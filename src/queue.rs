use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{self, AtomicU8, AtomicU16, AtomicUsize, Ordering};

use crossbeam_utils::CachePadded;

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

#[cfg(all(test, unix))]
static CRASH_REPAIR: AtomicU8 = AtomicU8::new(0);

#[cfg(all(test, unix))]
pub(crate) fn crash_repair_for_test() {
    CRASH_REPAIR.store(1, Ordering::Relaxed);
}

const _: () = {
    assert!(crate::header::PARTICIPANT_CAPACITY <= 63);
    assert!(BASE_MASK & (COMPLETE | SOURCE_MASK | REAPER_MASK | RESERVED_MASK) == 0);
    assert!(COMPLETE & (SOURCE_MASK | REAPER_MASK | RESERVED_MASK) == 0);
    assert!(SOURCE_MASK & (REAPER_MASK | RESERVED_MASK) == 0);
    assert!(REAPER_MASK & RESERVED_MASK == 0);
};

impl<T> Slot<T> {
    pub(crate) const fn new(turn: usize) -> Self {
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
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
pub struct Reserved<'a, T> {
    slot: &'a Slot<T>,
    done: bool,
}

#[must_use]
pub struct Staged<'a, T> {
    reserved: Reserved<'a, T>,
}

impl<'a, T> Reserved<'a, T> {
    pub fn stage(self, value: T) -> Staged<'a, T> {
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

impl<T> Drop for Reserved<'_, T> {
    fn drop(&mut self) {
        if !self.done {
            self.skip();
        }
    }
}

impl<T> Staged<'_, T> {
    pub fn publish(mut self) {
        self.reserved
            .slot
            .control
            .store(AVAILABLE | COMPLETE, Ordering::Release);
        self.reserved.done = true;
    }

    pub(crate) fn cancel(mut self) -> T {
        let value = unsafe { self.reserved.slot.value.get().read().assume_init() };
        self.reserved.skip();
        value
    }
}

impl<T> Drop for Staged<'_, T> {
    fn drop(&mut self) {
        if !self.reserved.done {
            unsafe { (*self.reserved.slot.value.get()).assume_init_drop() };
            self.reserved.skip();
        }
    }
}

#[must_use]
pub struct Claim<'a, T> {
    slot: &'a Slot<T>,
    next_turn: usize,
    initialized: bool,
    done: bool,
}

impl<T> Claim<'_, T> {
    pub(crate) fn item(&self) -> &T {
        debug_assert!(self.initialized);
        unsafe { (*self.slot.value.get()).assume_init_ref() }
    }

    fn recycle(&mut self) {
        self.slot.turn.store(self.next_turn, Ordering::Release);
        self.slot.control.store(EMPTY, Ordering::Release);
        self.done = true;
    }

    pub fn take(mut self) -> T {
        debug_assert!(self.initialized);
        let value = unsafe { self.slot.value.get().read().assume_init() };
        self.initialized = false;
        self.recycle();
        value
    }
}

impl<T> Drop for Claim<'_, T> {
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

    fn reserve(&self) -> Result<Reserved<'_, Self::Item>, ReserveError>
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
            return Err(ReserveError::Busy);
        }
        Ok(Reserved { slot, done: false })
    }

    fn claim(&self) -> Result<Claim<'_, Self::Item>, ClaimError>
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

    #[cfg(test)]
    fn repair(&self, index: usize, dead: u8, live: u8) -> Repair {
        repair_slot(self.header(), self.buf(), self.one_lap(), index, dead, live)
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
            3
        } else {
            state
        }
    }

    fn close_send(&self) {
        self.close_field(self.send_field());
    }

    fn close_recv(&self) {
        self.close_field(self.recv_field());
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

#[cfg(test)]
pub(crate) fn repair_slot<T>(
    header: &Header,
    buf: &[Slot<T>],
    one_lap: usize,
    index: usize,
    dead: u8,
    live: u8,
) -> Repair {
    repair_slot_with(header, buf, one_lap, index, dead, live, |_, _| true)
}

pub(crate) fn repair_slot_with<T>(
    header: &Header,
    buf: &[Slot<T>],
    one_lap: usize,
    index: usize,
    dead: u8,
    live: u8,
    mut release: impl FnMut(&T, u8) -> bool,
) -> Repair {
    if dead as usize >= crate::header::PARTICIPANT_CAPACITY
        || live as usize >= crate::header::PARTICIPANT_CAPACITY
    {
        return Repair::Corrupted;
    }
    let Some(slot) = buf.get(index) else {
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
    #[cfg(all(test, unix))]
    if CRASH_REPAIR.load(Ordering::Relaxed) != 0 {
        std::process::exit(95);
    }
    let complete = control & COMPLETE;
    match base(control) {
        PRODUCER => {
            let turn = slot.turn.load(Ordering::Acquire);
            let distance = header.tail.load(Ordering::Acquire).wrapping_sub(turn);
            if distance == 0 {
                if complete != 0 {
                    return Repair::Corrupted;
                }
                slot.control.store(EMPTY, Ordering::Release);
            } else if distance < 1usize << (usize::BITS - 1) {
                if complete != 0
                    && !release(unsafe { (*slot.value.get()).assume_init_ref() }, source - 1)
                {
                    return Repair::Corrupted;
                }
                slot.control.store(AVAILABLE, Ordering::Release);
            } else {
                return Repair::Corrupted;
            }
        }
        CONSUMER => {
            let turn = slot.turn.load(Ordering::Acquire);
            let head = header.head.load(Ordering::Acquire);
            if head == turn {
                slot.control.store(AVAILABLE | complete, Ordering::Release);
            } else {
                if complete != 0
                    && !release(unsafe { (*slot.value.get()).assume_init_ref() }, source - 1)
                {
                    return Repair::Corrupted;
                }
                let distance = head.wrapping_sub(turn);
                if distance != 0 && distance < 1usize << (usize::BITS - 1) {
                    slot.turn
                        .store(turn.wrapping_add(one_lap), Ordering::Release);
                }
                slot.control.store(EMPTY, Ordering::Release);
            }
        }
        _ => unreachable!(),
    }
    Repair::Recovered
}

#[derive(Debug)]
pub enum TrySendError<T> {
    /// Shared capacity is exhausted; receiver progress is required.
    Full(T),
    /// Local retry may progress without receiver action.
    Busy(T),
    Disconnected(T),
}

#[derive(Debug)]
#[cfg(test)]
pub enum TryRecvError {
    /// No committed item exists; sender progress is required.
    Empty,
    /// Local retry may progress without sender action.
    Busy,
    Disconnected,
}

#[derive(Clone, Debug, PartialEq)]
#[repr(transparent)]
#[cfg(test)]
pub struct QueueTx<T: Queue> {
    tx: T,
}

#[derive(Clone, Debug, PartialEq)]
#[repr(transparent)]
#[cfg(test)]
pub struct QueueRx<T: Queue> {
    rx: T,
}

#[cfg(test)]
impl<T: Queue> QueueTx<T> {
    pub fn reserve(&self) -> Result<Reserved<'_, T::Item>, ReserveError> {
        self.tx.reserve()
    }

    #[inline(always)]
    pub fn try_send(&self, value: T::Item) -> Result<(), TrySendError<T::Item>> {
        match self.reserve() {
            Ok(reserved) => {
                reserved.stage(value).publish();
                Ok(())
            }
            Err(ReserveError::Closed) => Err(TrySendError::Disconnected(value)),
            Err(ReserveError::Full) => Err(TrySendError::Full(value)),
            Err(ReserveError::Busy) => Err(TrySendError::Busy(value)),
        }
    }
}

#[cfg(test)]
impl<T: Queue> QueueRx<T> {
    pub fn claim(&self) -> Result<Claim<'_, T::Item>, ClaimError> {
        self.rx.claim()
    }

    #[inline(always)]
    pub fn try_recv(&self) -> Result<T::Item, TryRecvError> {
        match self.claim() {
            Ok(claim) => Ok(claim.take()),
            Err(ClaimError::Closed) => Err(TryRecvError::Disconnected),
            Err(ClaimError::Empty) if self.rx.terminal() => Err(TryRecvError::Disconnected),
            Err(ClaimError::Empty) => Err(TryRecvError::Empty),
            Err(ClaimError::Busy) => Err(TryRecvError::Busy),
        }
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::{
        AVAILABLE, BASE_MASK, COMPLETE, CONSUMER, EMPTY, Header, PRODUCER, Queue, QueueRx, QueueTx,
        REAPER_MASK, RESERVED_MASK, SOURCE_MASK, Slot, TryRecvError, TrySendError, canonical,
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
    fn try_send_preserves_local_contention_and_exact_item() {
        let queue = TestQueue::new(1);
        queue.slots[0]
            .control
            .store(super::owned(PRODUCER, false, 0), Ordering::Release);
        let sender = QueueTx { tx: &queue };

        match sender.try_send(43) {
            Err(TrySendError::Busy(value)) => assert_eq!(value, 43),
            other => panic!("local producer contention must stay Busy: {other:?}"),
        }
    }

    #[test]
    fn try_recv_preserves_local_contention() {
        let queue = TestQueue::new(1);
        queue.slots[0]
            .control
            .store(super::owned(PRODUCER, false, 0), Ordering::Release);
        queue.header.tail.store(1, Ordering::Release);
        let receiver = QueueRx { rx: &queue };

        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Busy)));
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
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Busy)));
        QueueTx { tx: &queue }.try_send(23).unwrap();
        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Busy)));
        assert_eq!(receiver.try_recv().unwrap(), 23);
    }

    #[test]
    fn non_power_of_two_capacity_wraps_without_reordering() {
        let queue = TestQueue::new(3);
        let sender = QueueTx { tx: &queue };
        let receiver = QueueRx { rx: &queue };

        for lap in 0..16 {
            for index in 0..3 {
                sender.try_send(lap * 3 + index).unwrap();
            }
            assert!(matches!(
                sender.try_send(u64::MAX),
                Err(TrySendError::Full(u64::MAX))
            ));
            for index in 0..3 {
                assert_eq!(receiver.try_recv().unwrap(), lap * 3 + index);
            }
            assert!(matches!(receiver.try_recv(), Err(TryRecvError::Empty)));
        }
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
                    let mut pending = value;
                    loop {
                        match (QueueTx { tx: queue }).try_send(pending) {
                            Ok(()) => {
                                accepted.fetch_add(1, Ordering::Relaxed);
                                break;
                            }
                            Err(TrySendError::Busy(returned)) => {
                                pending = returned;
                                std::thread::yield_now();
                            }
                            Err(TrySendError::Disconnected(returned))
                            | Err(TrySendError::Full(returned)) => {
                                assert_eq!(returned, value);
                                rejected.fetch_add(1, Ordering::Relaxed);
                                break;
                            }
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
                Err(super::TryRecvError::Empty | super::TryRecvError::Busy) => {
                    std::thread::yield_now()
                }
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
        unsafe { (*queue.slots[0].value.get()).write(37) };
        queue.slots[0].control.store(
            super::owned(PRODUCER, true, 0) | (2 << super::REAPER_SHIFT),
            Ordering::Release,
        );
        queue.header.tail.store(1, Ordering::Release);

        let mut source = None;
        assert_eq!(
            super::repair_slot_with(
                &queue.header,
                &queue.slots,
                queue.one_lap,
                0,
                1,
                2,
                |_, owner| {
                    source = Some(owner);
                    true
                },
            ),
            super::Repair::Recovered
        );
        assert_eq!(source, Some(0));
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
    fn repair_cancels_staged_but_unpublished_bytes() {
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
        sent.header.tail.store(2, Ordering::Release);
        assert_eq!((&sent).repair(0, 0, 1), super::Repair::Recovered);
        let receiver = QueueRx { rx: &sent };
        assert!(matches!(
            receiver.try_recv(),
            Err(super::TryRecvError::Busy)
        ));
        assert!(matches!(
            receiver.try_recv(),
            Err(super::TryRecvError::Empty)
        ));

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
