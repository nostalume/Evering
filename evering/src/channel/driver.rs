use core::{
    cell::UnsafeCell,
    clone::Clone,
    future::Future,
    mem::MaybeUninit,
    sync::atomic::{AtomicU8, AtomicU32, AtomicUsize, Ordering, fence},
    task::{Context, Poll, Waker},
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

const HEAD: usize = 0;
const NONE: usize = usize::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Id {
    idx: usize,
    live: u32,
}

impl Id {
    pub const fn null() -> Self {
        Self { idx: NONE, live: 0 }
    }

    pub const fn is_null(&self) -> bool {
        self.idx == NONE
    }
}

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

pub struct Op<'a, T, const N: usize> {
    pool: &'a CachePool<T, N>,
    entry: &'a Cache<T>,
    idx: usize,
}

impl<'a, T, const N: usize> Future for Op<'a, T, N> {
    type Output = T;

    fn poll(self: core::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.entry.poll(cx)
    }
}

impl<'a, T, const N: usize> Drop for Op<'a, T, N> {
    fn drop(&mut self) {
        unsafe { self.entry.clean() };
        self.pool.push_free(self.idx)
    }
}

pub struct CachePool<T, const N: usize> {
    inits: AtomicUsize,
    free_head: AtomicUsize,
    entries: [Cache<T>; N],
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
}

impl<T, const N: usize> CachePool<T, N> {
    pub fn register(&self) -> Option<(Id, Op<'_, T, N>)> {
        let idx = self.pop_free();
        if idx == NONE {
            return None;
        }
        let entry = &self.entries[idx];
        let live = entry.live.load(Ordering::Relaxed);
        Some((
            Id { idx, live },
            Op {
                entry,
                pool: self,
                idx,
            },
        ))
    }

    pub fn complete(&self, id: Id, payload: T) -> Option<bool> {
        // id must be bounded
        let e = &self.entries[id.idx];
        if e.live.load(Ordering::Relaxed) != id.live {
            return None;
        }
        Some(e.complete(payload))
    }
}

pub struct CachePoolHandle<T, const N: usize>(crate::counter::CounterOf<CachePool<T, N>>);

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
    fn new() -> Self {
        let pool = CachePool::new();
        Self(crate::counter::CounterOf::suspend(pool))
    }

    fn bind<S: super::Sender, R: super::Receiver>(
        self,
        sender: S,
        receiver: R,
    ) -> (Submitter<S, T, N>, Completer<R, T, N>) {
        let s = Submitter {
            sender,
            pool: self.clone(),
        };
        let c = Completer {
            receiver,
            pool: self.clone(),
        };
        (s, c)
    }
}

use crate::msg::{Envelope, Tag};
use crate::token::PackToken;

struct Submitter<S: super::Sender, T, const N: usize> {
    sender: S,
    pool: CachePoolHandle<T, N>,
}

impl<H, M, S, T, const N: usize> Submitter<S, T, N>
where
    S: super::Sender<Item = PackToken<H, M>>,
    H: Envelope + Tag<Id>,
{
    fn try_submit(&self, item: S::Item) -> Option<Op<'_, T, N>> {
        let (id, op) = self.pool.0.register()?;
        let item = item.with_tag(id);
        self.sender.try_send(item).ok()?;
        Some(op)
    }
}

struct Completer<R: super::Receiver, T, const N: usize> {
    receiver: R,
    pool: CachePoolHandle<T, N>,
}

impl<H, M, R, const N: usize> Completer<R, PackToken<H, M>, N>
where
    R: super::Receiver<Item = PackToken<H, M>>,
    H: Envelope + Tag<Id>,
{
    fn try_complete(&self) {
        if let Ok(token) = self.receiver.try_recv() {
            self.pool.0.complete(token.tag(), token);
        }
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
        use crate::tracing_init;
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
                let (id, op) = pool.register().expect("should allocate");
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

        let (id, _) = pool.register().expect("should allocate");
        assert_ne!(id.live, 0)
    }
}
