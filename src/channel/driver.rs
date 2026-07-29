use core::{
    cell::UnsafeCell,
    clone::Clone,
    future::Future,
    mem::MaybeUninit,
    ops::Deref,
    ptr,
    sync::atomic::{AtomicU8, AtomicU32, AtomicUsize, Ordering, fence},
    task::{Context, Poll, Waker},
};

use crate::{channel::QueueChannel, numeric::Id};

use crossbeam_utils::Backoff;

mod state {
    // FREE -> WAKER -> COMPLETED -> FREE
    /// FREE: at initiation
    pub const FREE: u8 = 0;
    /// WAKER: with `waker`, without `payload`
    pub const WAKER: u8 = 1;
    /// UPDATING: update `waker`
    pub const UPDATING: u8 = 2;
    /// COMPLETED: with `payload`, possibly with `waker`
    pub const COMPLETED: u8 = 3;
    pub const COMPLETING: u8 = 4;
    pub const CLEANING: u8 = 5;
    pub const RETIRED: u8 = 6;
}

const HEAD: usize = Id::HEAD;
const NONE: usize = Id::NONE;

#[repr(C)]
struct Cache<T> {
    next_free: AtomicUsize,
    live: AtomicU32,
    state: AtomicU8,
    waker: UnsafeCell<MaybeUninit<Waker>>,
    payload: UnsafeCell<MaybeUninit<T>>,
}

unsafe impl<T: Send> Send for Cache<T> {}
unsafe impl<T: Send> Sync for Cache<T> {}

#[derive(Debug, PartialEq)]
pub enum Completion<T> {
    Stored {
        woke: bool,
    },
    Rejected {
        reason: CompletionReject,
        payload: T,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionReject {
    Prefilled,
    Outdated,
    Retired,
}

impl<T> Cache<T> {
    pub const fn null(next_free: usize) -> Self {
        Self {
            next_free: AtomicUsize::new(next_free),
            live: AtomicU32::new(0),
            state: AtomicU8::new(state::FREE),
            waker: UnsafeCell::new(MaybeUninit::uninit()),
            payload: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    const fn array<const N: usize>() -> [Self; N] {
        let mut arr = [const { Cache::null(NONE) }; N];
        let mut i = HEAD;
        while i < N - 1 {
            arr[i] = Cache::null(i + 1);
            i += 1
        }

        arr
    }

    unsafe fn drop_waker(&self) {
        unsafe {
            let _ = &(*self.waker.get()).assume_init_drop();
        }
    }

    unsafe fn write_waker(&self, ctx: &mut Context<'_>) {
        unsafe {
            let _ = &(*self.waker.get()).write(ctx.waker().clone());
        }
    }

    unsafe fn read_waker(&self) -> &Waker {
        unsafe { (*self.waker.get()).assume_init_ref() }
    }

    unsafe fn replace_payload(&self, value: T) -> MaybeUninit<T> {
        unsafe { self.payload.replace(MaybeUninit::new(value)) }
    }

    unsafe fn take_payload(&self) -> T {
        let val = unsafe { self.payload.replace(MaybeUninit::uninit()) };
        unsafe { val.assume_init() }
    }

    unsafe fn drop_payload(&self) {
        unsafe {
            let _ = &(*self.payload.get()).assume_init_drop();
        }
    }

    pub fn complete(&self, live: u32, payload: T) -> Completion<T> {
        let backoff = Backoff::new();
        loop {
            let current = self.state.load(Ordering::Acquire);
            match current {
                state::COMPLETED => {
                    return Completion::Rejected {
                        reason: CompletionReject::Prefilled,
                        payload,
                    };
                }
                state::RETIRED => {
                    return Completion::Rejected {
                        reason: CompletionReject::Retired,
                        payload,
                    };
                }
                state::UPDATING | state::COMPLETING | state::CLEANING => {
                    backoff.snooze();
                    continue;
                }
                state::FREE | state::WAKER => {}
                _ => unreachable!("invalid cache state"),
            }
            if self
                .state
                .compare_exchange_weak(
                    current,
                    state::COMPLETING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_err()
            {
                backoff.spin();
                continue;
            }
            if self.live.load(Ordering::Acquire) != live {
                self.state.store(current, Ordering::Release);
                return Completion::Rejected {
                    reason: CompletionReject::Outdated,
                    payload,
                };
            }
            unsafe { self.replace_payload(payload) };
            self.state.store(state::COMPLETED, Ordering::Release);
            let woke = current == state::WAKER;
            if woke {
                fence(Ordering::Acquire);
                let waker = unsafe { self.read_waker() };
                waker.wake_by_ref();
                unsafe { self.drop_waker() };
            }
            return Completion::Stored { woke };
        }
    }

    pub fn poll(&self, ctx: &mut Context<'_>) -> Poll<T> {
        let backoff = Backoff::new();

        loop {
            let cur = self.state.load(Ordering::Acquire);

            match cur {
                state::FREE | state::WAKER => {
                    if self
                        .state
                        .compare_exchange_weak(
                            cur,
                            state::UPDATING,
                            Ordering::Acquire,
                            Ordering::Relaxed,
                        )
                        .is_ok()
                    {
                        if cur == state::WAKER {
                            let old = unsafe { self.read_waker() };
                            if !old.will_wake(ctx.waker()) {
                                unsafe {
                                    self.drop_waker();
                                    self.write_waker(ctx);
                                }
                            }
                        } else {
                            unsafe { self.write_waker(ctx) }
                        }

                        if self
                            .state
                            .compare_exchange_weak(
                                state::UPDATING,
                                state::WAKER,
                                Ordering::Release,
                                Ordering::Relaxed,
                            )
                            .is_err()
                        {
                            unsafe {
                                self.drop_waker();
                            }
                            backoff.snooze();
                            continue;
                        }
                        return Poll::Pending;
                    }
                }
                state::COMPLETED => {
                    if self
                        .state
                        .compare_exchange_weak(
                            state::COMPLETED,
                            state::FREE,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        let payload = unsafe { self.take_payload() };
                        return Poll::Ready(payload);
                    } else {
                        backoff.snooze();
                        continue;
                    }
                }
                state::UPDATING => {
                    backoff.snooze();
                    continue;
                }
                _ => return Poll::Pending,
            }
        }
    }

    pub unsafe fn clean(&self) -> Option<u32> {
        let backoff = Backoff::new();
        let current = loop {
            let current = self.state.load(Ordering::Acquire);
            if matches!(
                current,
                state::UPDATING | state::COMPLETING | state::CLEANING
            ) {
                backoff.snooze();
                continue;
            }
            if current == state::RETIRED {
                return None;
            }
            if self
                .state
                .compare_exchange_weak(
                    current,
                    state::CLEANING,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                break current;
            }
        };
        if current == state::WAKER {
            unsafe { self.drop_waker() };
        } else if current == state::COMPLETED {
            unsafe { self.drop_payload() };
        }
        let Some(new_live) = self.live.load(Ordering::Relaxed).checked_add(1) else {
            self.state.store(state::RETIRED, Ordering::Release);
            return None;
        };
        self.live.store(new_live, Ordering::Release);
        self.state.store(state::FREE, Ordering::Release);
        Some(new_live)
    }

    #[cfg(test)]
    fn state(&self) -> u8 {
        self.state.load(Ordering::Acquire)
    }
}

pub struct Op<T, const N: usize, P: const Deref<Target = CachePool<T, N>>> {
    pool: P,
    entry: ptr::NonNull<Cache<T>>,
    idx: usize,
}

unsafe impl<T: Send, const N: usize, P: const Deref<Target = CachePool<T, N>> + Send> Send
    for Op<T, N, P>
{
}
unsafe impl<T: Send, const N: usize, P: const Deref<Target = CachePool<T, N>> + Sync> Sync
    for Op<T, N, P>
{
}

pub type RefOp<'a, T, const N: usize> = Op<T, N, &'a CachePool<T, N>>;
pub type OwnOp<T, const N: usize> = Op<T, N, CachePoolHandle<T, N>>;

impl<T, const N: usize, P: const Deref<Target = CachePool<T, N>>> Future for Op<T, N, P> {
    type Output = T;

    fn poll(self: core::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // Safety: ensured by `pool` field
        unsafe { self.entry.as_ref().poll(cx) }
    }
}

impl<T, const N: usize, P: const Deref<Target = CachePool<T, N>>> Drop for Op<T, N, P> {
    fn drop(&mut self) {
        // Safety: ensured by `pool` field
        if unsafe { self.entry.as_ref().clean() }.is_some() {
            self.pool.push_free(self.idx)
        }
    }
}

impl<T, const N: usize, P: const Deref<Target = CachePool<T, N>>> PartialEq for Op<T, N, P> {
    fn eq(&self, other: &Self) -> bool {
        self.entry == other.entry && self.idx == other.idx
    }
}

pub struct CachePool<T, const N: usize> {
    inits: AtomicUsize,
    free_head: AtomicUsize,
    entries: [Cache<T>; N],
}

impl<T, const N: usize> core::fmt::Debug for CachePool<T, N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let inits = self.inits.load(Ordering::Relaxed);
        f.debug_struct("CachePool")
            .field("inits", &inits)
            .field("entries", &"{ .. }")
            .finish()
    }
}

impl<T, const N: usize> CachePool<T, N> {
    pub const fn new() -> Self {
        Self {
            inits: AtomicUsize::new(0),
            free_head: AtomicUsize::new(HEAD),
            entries: const { Cache::array() },
        }
    }

    #[inline]
    pub const fn capacity(&self) -> usize {
        N
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.inits.load(Ordering::Relaxed)
    }

    fn pop_free(&self) -> usize {
        let backoff = Backoff::new();
        loop {
            let head = self.free_head.load(Ordering::Acquire);
            if head == NONE {
                return NONE;
            }

            let next = self.entries[head].next_free.load(Ordering::Relaxed);
            if self
                .free_head
                .compare_exchange_weak(head, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                self.inits.fetch_add(1, Ordering::AcqRel);
                return head;
            }
            backoff.snooze();
        }
    }

    fn push_free(&self, idx: usize) {
        let backoff = Backoff::new();
        loop {
            let head = self.free_head.load(Ordering::Acquire);
            self.entries[idx].next_free.store(head, Ordering::Relaxed);
            if self
                .free_head
                .compare_exchange_weak(head, idx, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                self.inits.fetch_sub(1, Ordering::AcqRel);
                return;
            }
            backoff.snooze();
        }
    }

    fn prepare(&self) -> Option<(&Cache<T>, Id)> {
        let idx = self.pop_free();
        if idx == NONE {
            return None;
        }
        let entry = &self.entries[idx];
        let live = entry.live.load(Ordering::Relaxed);
        Some((entry, Id { idx, live }))
    }
}

impl<T, const N: usize> CachePool<T, N> {
    pub fn probe(&self) -> Option<(RefOp<'_, T, N>, Id)> {
        let (entry, id) = self.prepare()?;
        Some((
            RefOp {
                pool: self,
                entry: entry.into(),
                idx: id.idx,
            },
            id,
        ))
    }

    fn complete(&self, id: Id, payload: T) -> Completion<T> {
        let Some(entry) = self.entries.get(id.idx) else {
            return Completion::Rejected {
                reason: CompletionReject::Outdated,
                payload,
            };
        };
        entry.complete(id.live, payload)
    }
}

#[derive(Debug)]
pub struct CachePoolHandle<T, const N: usize>(crate::counter::CounterOf<CachePool<T, N>>);

impl<T, const N: usize> const Deref for CachePoolHandle<T, N> {
    type Target = CachePool<T, N>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T, const N: usize> Clone for CachePoolHandle<T, N> {
    fn clone(&self) -> Self {
        Self(self.0.acquire())
    }
}

impl<T, const N: usize> Drop for CachePoolHandle<T, N> {
    fn drop(&mut self) {
        unsafe { self.0.release() };
    }
}

impl<T, const N: usize> CachePoolHandle<T, N> {
    pub fn new() -> Self {
        let pool = CachePool::new();
        Self(crate::counter::CounterOf::suspend(pool))
    }

    pub fn claim(&self) -> Option<(OwnOp<T, N>, Id)> {
        let (entry, id) = self.0.prepare()?;
        Some((
            OwnOp {
                pool: self.clone(),
                entry: entry.into(),
                idx: id.idx,
            },
            id,
        ))
    }

    pub fn bind<S: super::Sender, R: super::Receiver>(
        self,
        sender: S,
        receiver: R,
    ) -> (Sx<S, T, N>, Cx<R, T, N>)
    where
        S::Item: Identifier<T>,
        R::Item: Identifier<T>,
    {
        let s = Sx {
            sender,
            pool: self.clone(),
        };
        let c = Cx {
            receiver,
            pool: self.clone(),
        };
        (s, c)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmitCause {
    Full,
    Disconnected,
}

#[derive(Debug)]
pub enum TrySubmitError<U> {
    SendRejected { cause: SubmitCause, item: U },
    CacheFull(U),
}

pub trait SendRejection<T> {
    fn into_parts(self) -> (SubmitCause, T);
}

impl<T> SendRejection<T> for super::TrySendError<T> {
    fn into_parts(self) -> (SubmitCause, T) {
        match self {
            super::TrySendError::Full(item) => (SubmitCause::Full, item),
            super::TrySendError::Disconnected(item) => (SubmitCause::Disconnected, item),
        }
    }
}

pub trait Identified<U>: Sized {
    fn compose(self, id: Id) -> U;
    fn decompose(output: U) -> (Self, Id);
}

pub trait Identifier<T>: Sized {
    fn decompose(self) -> (T, Id);
    fn compose(origin: T, id: Id) -> Self;
}

impl<T: Identified<U>, U> Identifier<T> for U {
    fn decompose(self) -> (T, Id) {
        T::decompose(self)
    }

    fn compose(origin: T, id: Id) -> Self {
        T::compose(origin, id)
    }
}

pub trait Submitter<Op: Future, U> {
    type Item: Identifier<U>;
    type Error;
    fn try_submit(&self, item: U) -> Result<Op, Self::Error>;
}

pub trait Completer<U> {
    type Item: Identifier<U>;
    type Error;
    fn complete(&self) -> Result<Completion<U>, Self::Error>;
}

#[derive(Debug)]
pub struct Sx<S: super::Sender, U, const N: usize>
where
    S::Item: Identifier<U>,
{
    sender: S,
    pool: CachePoolHandle<U, N>,
}

impl<S: super::Sender + Clone, U, const N: usize> Clone for Sx<S, U, N>
where
    S::Item: Identifier<U>,
{
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            pool: self.pool.clone(),
        }
    }
}

impl<S: super::Sender, U, const N: usize> Sx<S, U, N>
where
    S::Item: Identifier<U>,
    S::TryError: SendRejection<S::Item>,
{
    pub fn try_submit_ref<'a>(&'a self, item: U) -> Result<RefOp<'a, U, N>, TrySubmitError<U>> {
        let Some((op, id)) = self.pool.0.probe() else {
            return Err(TrySubmitError::CacheFull(item));
        };
        let msg = S::Item::compose(item, id);
        if let Err(error) = self.sender.try_send(msg) {
            let (cause, returned) = error.into_parts();
            let (item, returned_id) = returned.decompose();
            assert_eq!(returned_id, id, "sender returned a different operation");
            drop(op);
            return Err(TrySubmitError::SendRejected { cause, item });
        }
        Ok(op)
    }
}

impl<S: super::Sender + QueueChannel, U, const N: usize> super::QueueChannel for Sx<S, U, N>
where
    S::Item: Identifier<U>,
{
    type Handle = S::Handle;

    #[inline]
    fn handle(&self) -> &Self::Handle {
        self.sender.handle()
    }

    fn close(&self) {
        self.sender.close()
    }

    fn is_close(&self) -> bool {
        self.sender.is_close()
    }
}

impl<'a, S: super::Sender, U, const N: usize> Submitter<OwnOp<U, N>, U> for Sx<S, U, N>
where
    S::Item: Identifier<U>,
    S::TryError: SendRejection<S::Item>,
{
    type Item = S::Item;

    type Error = TrySubmitError<U>;

    fn try_submit(&self, item: U) -> Result<OwnOp<U, N>, Self::Error> {
        let Some((op, id)) = self.pool.claim() else {
            return Err(TrySubmitError::CacheFull(item));
        };
        let msg = S::Item::compose(item, id);
        if let Err(error) = self.sender.try_send(msg) {
            let (cause, returned) = error.into_parts();
            let (item, returned_id) = returned.decompose();
            assert_eq!(returned_id, id, "sender returned a different operation");
            drop(op);
            return Err(TrySubmitError::SendRejected { cause, item });
        }
        Ok(op)
    }
}

#[derive(Debug)]
pub struct Cx<R: super::Receiver, U, const N: usize>
where
    R::Item: Identifier<U>,
{
    receiver: R,
    pool: CachePoolHandle<U, N>,
}

impl<R: super::Receiver + QueueChannel + Clone, U, const N: usize> Clone for Cx<R, U, N>
where
    R::Item: Identifier<U>,
{
    fn clone(&self) -> Self {
        Self {
            receiver: self.receiver.clone(),
            pool: self.pool.clone(),
        }
    }
}

impl<R: super::Receiver + QueueChannel, U, const N: usize> super::QueueChannel for Cx<R, U, N>
where
    R::Item: Identifier<U>,
{
    type Handle = R::Handle;

    #[inline]
    fn handle(&self) -> &Self::Handle {
        self.receiver.handle()
    }

    fn close(&self) {
        self.receiver.close()
    }

    fn is_close(&self) -> bool {
        self.receiver.is_close()
    }
}

impl<'a, R: super::Receiver, U, const N: usize> Completer<U> for Cx<R, U, N>
where
    R::Item: Identifier<U>,
{
    type Item = R::Item;

    type Error = R::TryError;

    fn complete(&self) -> Result<Completion<U>, Self::Error> {
        let msg = self.receiver.try_recv()?;
        let (payload, id) = msg.decompose();
        Ok(self.pool.0.complete(id, payload))
    }
}

#[cfg(test)]
mod tests {
    use crate::channel::driver::CachePool;

    use super::{Cache, Completion, NONE, state};
    use alloc::sync::Arc;
    use core::pin::Pin;
    use core::sync::atomic::{AtomicUsize, Ordering};
    use core::task::{Context, Poll, Waker};

    #[test]
    fn cache_complete() {
        const VALUE: u32 = 8;

        let cache = Cache::<u32>::null(NONE);
        let completed = cache.complete(0, VALUE);
        assert_eq!(completed, Completion::Stored { woke: false });
        let waker = Waker::noop();
        let mut ctx = Context::from_waker(&waker);
        match cache.poll(&mut ctx) {
            Poll::Ready(v) => {
                assert_eq!(v, VALUE)
            }
            Poll::Pending => panic!("expected ready"),
        }

        let cache2 = Cache::<u32>::null(NONE);
        match cache2.poll(&mut ctx) {
            Poll::Ready(_) => panic!("expected pending"),
            Poll::Pending => {}
        }
        assert_eq!(cache2.state(), state::WAKER);
        let completed = cache2.complete(0, VALUE);
        assert_eq!(completed, Completion::Stored { woke: true });
        match cache2.poll(&mut ctx) {
            Poll::Ready(v) => assert_eq!(v, VALUE),
            Poll::Pending => panic!("expected ready"),
        }
    }

    #[test]
    fn thread_cache_complete() {
        use std::thread;

        const VALUE: u32 = 12;

        let cache = Arc::new(Cache::<u32>::null(NONE));
        let cache2 = cache.clone();

        let handle = std::thread::spawn(move || {
            let waker = Waker::noop();
            let mut ctx = Context::from_waker(&waker);
            loop {
                match cache2.poll(&mut ctx) {
                    Poll::Ready(v) => return v,
                    Poll::Pending => {
                        thread::yield_now();
                        continue;
                    }
                }
            }
        });

        // wait for poll
        thread::sleep(std::time::Duration::from_millis(4));

        let _ = cache.complete(0, VALUE);
        let v = handle.join().expect("consumer thread returned");
        assert_eq!(v, VALUE);
    }

    #[test]
    fn cache_clean() {
        struct Droppy(Arc<AtomicUsize>);
        impl Drop for Droppy {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }

        let counter = Arc::new(AtomicUsize::new(0));
        let droppy = Droppy(counter.clone());
        let cache = Cache::<Droppy>::null(NONE);
        let _ = cache.complete(0, droppy);
        unsafe {
            let new_live = cache.clean();
            assert!(new_live.unwrap() > 0)
        };
        assert_eq!(counter.load(Ordering::Relaxed), 1)
    }

    #[test]
    fn duplicate_completion_returns_the_rejected_payload() {
        let cache = Cache::<u32>::null(NONE);
        assert!(matches!(
            cache.complete(0, 7),
            super::Completion::Stored { .. }
        ));
        assert_eq!(
            cache.complete(0, 9),
            super::Completion::Rejected {
                reason: super::CompletionReject::Prefilled,
                payload: 9,
            }
        );
    }

    #[test]
    fn cancellation_racing_completion_has_one_payload_owner() {
        use std::sync::Barrier;
        use std::thread;

        struct Droppy(Arc<AtomicUsize>);
        impl Drop for Droppy {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }

        for _ in 0..64 {
            let drops = Arc::new(AtomicUsize::new(0));
            let cache = Arc::new(Cache::null(NONE));
            let barrier = Arc::new(Barrier::new(3));
            let complete_cache = cache.clone();
            let complete_barrier = barrier.clone();
            let payload_drops = drops.clone();
            let complete = thread::spawn(move || {
                complete_barrier.wait();
                complete_cache.complete(0, Droppy(payload_drops))
            });
            let clean_cache = cache.clone();
            let clean_barrier = barrier.clone();
            let clean = thread::spawn(move || {
                clean_barrier.wait();
                unsafe { clean_cache.clean() }
            });
            barrier.wait();
            let completion = complete.join().unwrap();
            assert!(clean.join().unwrap().is_some());
            if let Completion::Rejected { payload, .. } = completion {
                drop(payload);
            }
            assert_eq!(drops.load(Ordering::Relaxed), 1);
        }
    }

    struct Wire(u32, crate::numeric::Id);

    impl super::Identified<Wire> for u32 {
        fn compose(self, id: crate::numeric::Id) -> Wire {
            Wire(self, id)
        }
        fn decompose(output: Wire) -> (Self, crate::numeric::Id) {
            (output.0, output.1)
        }
    }

    struct RejectSender;

    impl crate::channel::Sender for RejectSender {
        type Item = Wire;
        type TryError = crate::channel::TrySendError<Wire>;
        fn try_send(&self, item: Wire) -> Result<(), Self::TryError> {
            Err(crate::channel::TrySendError::Full(item))
        }
    }

    #[test]
    fn submission_failures_return_the_original_item() {
        use super::{SubmitCause, Submitter, Sx, TrySubmitError};

        let submitter = Sx {
            sender: RejectSender,
            pool: super::CachePoolHandle::<u32, 1>::new(),
        };
        match submitter.try_submit(73) {
            Err(TrySubmitError::SendRejected {
                cause: SubmitCause::Full,
                item,
            }) => assert_eq!(item, 73),
            _ => panic!("send rejection must return original input"),
        }

        let held = submitter.pool.claim().unwrap().0;
        match submitter.try_submit(91) {
            Err(TrySubmitError::CacheFull(item)) => assert_eq!(item, 91),
            _ => panic!("cache exhaustion must return original input"),
        }
        drop(held);
    }

    #[test]
    fn pool() {
        use crate::tests::tracing_init;

        use std::sync::Barrier;
        use std::thread;

        const N: usize = 100;

        tracing_init();

        let pool = Arc::new(CachePool::<u32, N>::new());
        let bar = Arc::new(Barrier::new(2 * N));
        thread::scope(|s| {
            let mut ids = Vec::with_capacity(N);
            let mut ops = Vec::with_capacity(N);

            for _ in 0..N {
                let (op, id) = pool.probe().expect("should allocate");
                ids.push(id);
                ops.push(op);
            }

            let mut handles = Vec::with_capacity(2 * N);

            for id in ids {
                let pool = pool.clone();
                let bar = bar.clone();
                handles.push(s.spawn(move || {
                    bar.wait();
                    let tid = thread::current().id();
                    tracing::debug!("{:?} complete id: {:?}", tid, id);
                    thread::sleep(std::time::Duration::from_micros(fastrand::u64(50..300)));
                    pool.complete(id, fastrand::u32(0..100));
                }));
            }

            for mut op in ops {
                let bar = bar.clone();
                handles.push(s.spawn(move || {
                    let mut ctx = Context::from_waker(Waker::noop());
                    bar.wait();
                    loop {
                        match Pin::new(&mut op).poll(&mut ctx) {
                            Poll::Ready(v) => {
                                let tid = thread::current().id();
                                tracing::debug!("{:?} receive: {:?}", tid, v);
                                break;
                            }
                            Poll::Pending => {
                                thread::yield_now();
                                continue;
                            }
                        }
                    }
                }));
            }

            fastrand::shuffle(&mut handles);

            for h in handles {
                h.join().unwrap()
            }
        });

        let (_, id) = pool.probe().expect("should allocate");
        assert_ne!(id.live, 0)
    }
}
