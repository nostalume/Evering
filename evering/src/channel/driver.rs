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

use crate::{
    channel::{QueueChannel, Sender},
    numeric::Id,
};

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
unsafe impl<T: Sync> Sync for Cache<T> {}

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

    /// return complete successfully or not.
    pub fn complete(&self, payload: T) -> bool {
        unsafe { self.replace_payload(payload) };
        // AcqRel ensures visible
        let prev = self.state.swap(state::COMPLETED, Ordering::AcqRel);

        match prev {
            state::WAKER => {
                fence(Ordering::Acquire);
                let waker = unsafe { self.read_waker() };
                waker.wake_by_ref();
                unsafe { self.drop_waker() };
                true
            }
            state::UPDATING => {
                // overwrite UPDATING to COMPLETED
                // poll() will loop and detect failture to handle payload
                true
            }
            // state::FREE: no waker therefore false.
            _ => false,
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

    pub unsafe fn clean(&self) -> u32 {
        let cur = self.state.load(Ordering::Acquire);
        let new_live = self.live.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        if cur == state::WAKER {
            unsafe {
                self.drop_waker();
            }
        } else if cur == state::COMPLETED {
            unsafe {
                self.drop_payload();
            }
        }
        self.state.store(state::FREE, Ordering::Release);
        new_live
    }

    pub fn state(&self) -> u8 {
        self.state.load(Ordering::Acquire)
    }
}

pub struct Op<T, const N: usize, P: const Deref<Target = CachePool<T, N>>> {
    pool: P,
    entry: ptr::NonNull<Cache<T>>,
    idx: usize,
}

unsafe impl<T: Send, const N: usize, P: const Deref<Target = CachePool<T, N>>> Send
    for Op<T, N, P>
{
}
unsafe impl<T, const N: usize, P: const Deref<Target = CachePool<T, N>>> Sync for Op<T, N, P> {}

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
        unsafe { self.entry.as_ref().clean() };
        self.pool.push_free(self.idx)
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
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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

    fn lookup(&self, id: Id) -> Option<&Cache<T>> {
        let e = &self.entries[id.idx];
        if e.live.load(Ordering::Acquire) != id.live {
            return None;
        }
        Some(e)
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

    fn complete(&self, id: Id, payload: T) -> TryCompState {
        // id must be bounded
        let Some(e) = self.lookup(id) else {
            return TryCompState::Outdated;
        };
        if e.complete(payload) {
            TryCompState::Success
        } else {
            TryCompState::Prefilled
        }
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

#[derive(Debug)]
pub enum TrySubmitError<E> {
    SendError(E),
    CacheFull,
}

#[derive(Debug, PartialEq)]
pub enum TryCompState {
    Success,
    Prefilled,
    Outdated,
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
    fn complete(&self) -> Result<TryCompState, Self::Error>;
}

#[derive(Clone, Debug)]
pub struct Sx<S: super::Sender, U, const N: usize>
where
    S::Item: Identifier<U>,
{
    sender: S,
    pool: CachePoolHandle<U, N>,
}

impl<S: super::Sender, U, const N: usize> Sx<S, U, N>
where
    S::Item: Identifier<U>,
{
    pub fn try_submit_ref<'a>(
        &'a self,
        item: U,
    ) -> Result<RefOp<'a, U, N>, TrySubmitError<S::TryError>> {
        let (op, id) = self.pool.0.probe().ok_or(TrySubmitError::CacheFull)?;
        let msg = S::Item::compose(item, id);
        self.sender
            .try_send(msg)
            .map_err(TrySubmitError::SendError)?;
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
}

impl<'a, S: super::Sender, U, const N: usize> Submitter<OwnOp<U, N>, U> for Sx<S, U, N>
where
    S::Item: Identifier<U>,
{
    type Item = S::Item;

    type Error = TrySubmitError<S::TryError>;

    fn try_submit(&self, item: U) -> Result<OwnOp<U, N>, Self::Error> {
        let (op, id) = self.pool.claim().ok_or(TrySubmitError::CacheFull)?;
        let msg = S::Item::compose(item, id);
        self.sender
            .try_send(msg)
            .map_err(TrySubmitError::SendError)?;
        Ok(op)
    }
}

#[derive(Clone, Debug)]
pub struct Cx<R: super::Receiver, U, const N: usize>
where
    R::Item: Identifier<U>,
{
    receiver: R,
    pool: CachePoolHandle<U, N>,
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
}

impl<'a, R: super::Receiver, U, const N: usize> Completer<U> for Cx<R, U, N>
where
    R::Item: Identifier<U>,
{
    type Item = R::Item;

    type Error = R::TryError;

    fn complete(&self) -> Result<TryCompState, Self::Error> {
        let msg = self.receiver.try_recv()?;
        let (payload, id) = msg.decompose();
        Ok(self.pool.0.complete(id, payload))
    }
}

#[cfg(test)]
mod tests {
    use crate::channel::driver::CachePool;

    use super::{Cache, NONE, state};
    use alloc::sync::Arc;
    use core::pin::Pin;
    use core::sync::atomic::{AtomicUsize, Ordering};
    use core::task::{Context, Poll, Waker};

    #[test]
    fn cache_complete() {
        const VALUE: u32 = 8;

        let cache = Cache::<u32>::null(NONE);
        let waken = cache.complete(VALUE);
        assert_eq!(waken, false);
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
        let waken = cache2.complete(VALUE);
        assert_eq!(waken, true);
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

        let _ = cache.complete(VALUE);
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
        let _ = cache.complete(droppy);
        unsafe {
            let new_live = cache.clean();
            assert!(new_live > 0)
        };
        assert_eq!(counter.load(Ordering::Relaxed), 1)
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
