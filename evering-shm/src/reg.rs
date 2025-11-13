use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    ops::Deref,
    sync::atomic::{AtomicU8, AtomicU32, AtomicUsize, Ordering},
};

use crossbeam_utils::Backoff;

use crate::{
    header::Metadata,
    reg::state::{ACTIVE, INACTIVE},
};

const HEAD: usize = 0;
const NONE: usize = usize::MAX;

pub mod state {
    pub const FREE: u8 = 0;
    pub const INITIALIZING: u8 = 1;
    pub const ACTIVE: u8 = 2;
    pub const INACTIVE: u8 = 3;
    pub const DEINITIALIZING: u8 = 4;
}

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
struct Entry<T> {
    data: UnsafeCell<MaybeUninit<T>>,
    rc: AtomicUsize,
    next_free: AtomicUsize,
    live: AtomicU32,
    state: AtomicU8,
}

pub struct EntryGuard<'a, T> {
    entry: &'a Entry<T>,
    id: Id,
}

pub struct EntryView<'a, T: Project> {
    pub guard: EntryGuard<'a, T>,
    pub view: T::View,
}

unsafe impl<T: Send> Send for Entry<T> {}
unsafe impl<T: Sync> Sync for Entry<T> {}

impl<T> core::fmt::Debug for Entry<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self.state.load(Ordering::Relaxed) {
            0 => "FREE",
            1 => "INITIALIZING",
            2 => "ACTIVE",
            3 => "INACTIVE",
            4 => "DEINITIALIZING",
            _ => unreachable!(),
        };
        f.debug_struct("Entry")
            .field("ref counts", &self.rc.load(Ordering::Relaxed))
            .field("state", &s)
            .finish()
    }
}

impl<T> Entry<T> {
    const fn null(next_free: usize) -> Self {
        Self {
            data: UnsafeCell::new(MaybeUninit::uninit()),
            rc: AtomicUsize::new(0),
            next_free: AtomicUsize::new(next_free),
            state: AtomicU8::new(state::FREE),
            live: AtomicU32::new(0),
        }
    }

    const fn array<const N: usize>() -> [Self; N] {
        let mut arr = [const { Entry::null(NONE) }; N];
        let mut i = HEAD;
        while i < N - 1 {
            arr[i] = Entry::null(i + 1);
            i += 1
        }

        arr
    }

    #[inline]
    unsafe fn take(&self) -> T {
        unsafe { self.data.replace(MaybeUninit::uninit()).assume_init() }
    }

    #[inline]
    unsafe fn as_ref(&self) -> &T {
        unsafe { (*self.data.get()).assume_init_ref() }
    }

    #[inline]
    unsafe fn as_mut(&self) -> &mut T {
        unsafe { (*self.data.get()).assume_init_mut() }
    }

    #[inline]
    unsafe fn write(&self, value: T) {
        unsafe { (*self.data.get()).write(value) };
    }

    fn alloc(&self, value: T) -> Result<u32, T> {
        if self
            .state
            .compare_exchange_weak(
                state::FREE,
                state::INITIALIZING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return Err(value);
        }

        unsafe {
            self.write(value);
        }

        // state suggests that rc is 0.
        self.rc.store(0, Ordering::Relaxed);
        self.state.store(state::INACTIVE, Ordering::Release);
        let new_live = self.live.fetch_add(1, Ordering::AcqRel).wrapping_add(1);
        Ok(new_live)
    }

    #[inline]
    fn initiated(state: u8) -> bool {
        state == INACTIVE || state == ACTIVE
    }

    fn lookup(&self) -> Option<u32> {
        let state = self.state.load(Ordering::Acquire);
        if !Self::initiated(state) {
            return None;
        }
        let live = self.live.load(Ordering::Relaxed);
        Some(live)
    }

    fn acquire<'a>(&'a self, id: Id) -> Option<EntryGuard<'a, T>> {
        let backoff = Backoff::new();

        let live = self.live.load(Ordering::Acquire);
        if id.live != live {
            return None;
        }

        loop {
            let state = self.state.load(Ordering::Acquire);
            match state {
                state::ACTIVE => {}
                state::INACTIVE => {
                    if self
                        .state
                        .compare_exchange_weak(
                            state::INACTIVE,
                            state::ACTIVE,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_err()
                    {
                        backoff.snooze();
                        continue;
                    }
                }
                _ => return None,
            }
            let rc = self.rc.load(Ordering::Relaxed);
            if rc == usize::MAX {
                return None;
            }

            if self
                .rc
                .compare_exchange_weak(rc, rc + 1, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                if self.live.load(Ordering::Acquire) != id.live {
                    self.rc.fetch_sub(1, Ordering::AcqRel);
                    return None;
                }
                let state = self.state.load(Ordering::Acquire);
                if !Self::initiated(state) {
                    self.rc.fetch_sub(1, Ordering::AcqRel);
                    return None;
                }
                return Some(EntryGuard { entry: self, id });
            }
            backoff.snooze();
        }
    }

    fn free(&self, id: Id) -> Option<T> {
        if id.live != self.live.load(Ordering::Acquire) {
            return None;
        }

        if self
            .state
            .compare_exchange_weak(
                state::INACTIVE,
                state::DEINITIALIZING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return None;
        }

        // We are the sole owner.
        let data = unsafe { self.take() };

        self.state.store(state::FREE, Ordering::Release);
        Some(data)
    }
}

impl<T> Clone for EntryGuard<'_, T> {
    fn clone(&self) -> Self {
        self.entry.rc.fetch_add(1, Ordering::AcqRel);
        Self {
            entry: self.entry,
            id: self.id,
        }
    }
}

impl<T> Drop for EntryGuard<'_, T> {
    fn drop(&mut self) {
        let prev = self.entry.rc.fetch_sub(1, Ordering::AcqRel);
        if prev == 1 {
            // state must be `ACTIVE` -> `INACTIVE`, ensure ordering release.
            self.entry.state.store(state::INACTIVE, Ordering::Release);
        }
    }
}

impl<T> Deref for EntryGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl<T> core::fmt::Debug for EntryGuard<'_, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EntryGuard")
            .field("entry", self.entry)
            .finish()
    }
}

impl<T: core::fmt::Debug> core::fmt::Display for EntryGuard<'_, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EntryGuard")
            .field("entry", self.entry)
            .field("value", self.as_ref())
            .finish()
    }
}

impl<T> EntryGuard<'_, T> {
    pub fn rc(e: &Self) -> usize {
        e.entry.rc.load(Ordering::Relaxed)
    }

    #[inline(always)]
    pub fn as_ref(&self) -> &T {
        unsafe { self.entry.as_ref() }
    }

    #[inline(always)]
    pub unsafe fn as_mut(&self) -> &mut T {
        unsafe { self.entry.as_mut() }
    }
}

impl<T: Project> Clone for EntryView<'_, T>
where
    T::View: Clone,
{
    #[inline]
    fn clone(&self) -> Self {
        Self {
            guard: self.guard.clone(),
            view: self.view.clone(),
        }
    }
}

impl<T: Project> core::fmt::Debug for EntryView<'_, T>
where
    T::View: core::fmt::Debug,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EntryView")
            .field("entry", &self.guard)
            .field("view", &self.view)
            .finish()
    }
}

#[repr(C)]
pub struct Registry<T, const N: usize> {
    magic: crate::header::MAGIC,
    inits: AtomicUsize,
    free_head: AtomicUsize,
    entries: [Entry<T>; N],
}

impl<T, const N: usize> core::fmt::Debug for Registry<T, N> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let inits = self.inits.load(Ordering::Relaxed);
        f.debug_struct("Registry")
            .field("initiated entries:", &inits)
            .field("entries:", &"{ .. }")
            .finish()
    }
}

impl<T, const N: usize> crate::header::Layout for Registry<T, N> {
    type Config = ();
    #[inline]
    fn init(&mut self, _cfg: ()) -> crate::header::HeaderStatus {
        let ptr = self as *mut Self;
        unsafe { ptr.write(Self::new()) };

        crate::header::HeaderStatus::Initialized
    }

    #[inline]
    fn attach(&self) -> crate::header::HeaderStatus {
        if self.valid_magic() {
            crate::header::HeaderStatus::Initialized
        } else {
            crate::header::HeaderStatus::Uninitialized
        }
    }
}

impl<T, const N: usize> crate::header::Metadata for Registry<T, N> {
    const MAGIC_VALUE: crate::header::MAGIC = 0x1234;

    #[inline]
    fn valid_magic(&self) -> bool {
        self.magic == Self::MAGIC_VALUE
    }

    #[inline]
    fn with_magic(&mut self) {
        self.magic = Self::MAGIC_VALUE
    }
}

impl<T, const N: usize> Registry<T, N> {
    pub const fn new() -> Self {
        Self {
            magic: Self::MAGIC_VALUE,
            inits: AtomicUsize::new(0),
            free_head: AtomicUsize::new(HEAD),
            entries: const { Entry::array() },
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

    #[inline]
    fn alloc(r: &Self, value: T) -> Result<Id, T> {
        let idx = r.pop_free();
        if idx == NONE {
            return Err(value);
        }
        let e = &r.entries[idx];
        let new_live = e.alloc(value)?;
        Ok(Id {
            idx,
            live: new_live,
        })
    }

    #[inline]
    fn lookup(&self, idx: usize) -> Option<Id> {
        if idx >= N {
            return None;
        }

        let e = &self.entries[idx];
        let live = e.lookup()?;
        Some(Id { idx, live })
    }

    #[inline]
    fn acquire<'a>(r: &'a Self, id: Id) -> Option<EntryGuard<'a, T>> {
        let e = &r.entries[id.idx];
        e.acquire(id)
    }

    #[inline]
    fn free(r: &Self, id: Id) -> Option<T> {
        let idx = id.idx;
        let e = &r.entries[idx];
        let data = e.free(id)?;
        r.push_free(idx);
        Some(data)
    }
}

pub trait Resource: Sized {
    type Config: Clone;
    type Ctx;
    fn new(cfg: Self::Config, ctx: Self::Ctx) -> (Self, Self::Ctx);
    fn free(s: Self, ctx: Self::Ctx) -> Self::Ctx;
}

pub trait Project: Resource {
    type View;
    fn project(&self, ctx: Self::Ctx) -> (Self::View, Self::Ctx);
}

impl<T: Resource, const N: usize> Registry<T, N> {
    pub fn prepare(&self, cfg: T::Config, ctx: T::Ctx) -> Result<(Id, T::Ctx), T::Ctx> {
        let (value, ctx) = T::new(cfg.clone(), ctx);
        let id = match Self::alloc(self, value) {
            Ok(id) => id,
            Err(value) => return Err(T::free(value, ctx)),
        };
        Ok((id, ctx))
    }

    pub fn clear(&self, id: Id, ctx: T::Ctx) -> T::Ctx {
        if let Some(value) = Self::free(self, id) {
            T::free(value, ctx)
        } else {
            ctx
        }
    }
}

impl<T: Project, const N: usize> Registry<T, N> {
    pub fn view<'a>(&'a self, id: Id, ctx: T::Ctx) -> (Option<EntryView<'a, T>>, T::Ctx) {
        let Some(g) = Registry::acquire(self, id) else {
            return (None, ctx);
        };
        let (v, ctx) = g.as_ref().project(ctx);
        (Some(EntryView { guard: g, view: v }), ctx)
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicUsize, Ordering};

    use crate::{reg::EntryGuard, tracing_init};

    use super::{Project, Registry, Resource};

    fn mock_reg<T, const N: usize>() -> Arc<Registry<T, N>> {
        Registry::new().into()
    }

    struct MockResource {
        id: usize,
        // It should be clarified that
        // to share process-invariant data
        // the heap allocation context is not allowed
        // It's only test-oriented.
        init_count: Arc<AtomicUsize>,
        drop_count: Arc<AtomicUsize>,
    }

    impl MockResource {
        fn mock_id() -> usize {
            fastrand::usize(0..100)
        }
        fn mock_ctx() -> (Arc<AtomicUsize>, Arc<AtomicUsize>) {
            (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)))
        }
    }

    impl Resource for MockResource {
        type Config = usize;
        type Ctx = (Arc<AtomicUsize>, Arc<AtomicUsize>);

        fn new(cfg: Self::Config, ctx: Self::Ctx) -> (Self, Self::Ctx) {
            use core::sync::atomic::Ordering;
            ctx.0.fetch_add(1, Ordering::SeqCst);
            (
                MockResource {
                    id: cfg,
                    init_count: ctx.0.clone(),
                    drop_count: ctx.1.clone(),
                },
                ctx,
            )
        }

        fn free(_s: Self, ctx: Self::Ctx) -> Self::Ctx {
            use core::sync::atomic::Ordering;
            ctx.1.fetch_add(1, Ordering::SeqCst);
            ctx
        }
    }

    impl Project for MockResource {
        type View = usize;
        fn project(&self, ctx: Self::Ctx) -> (Self::View, Self::Ctx) {
            (self.id, ctx)
        }
    }

    #[test]
    fn single_thread() {
        const N: usize = 8;
        const CONFIG: usize = 42;

        tracing_init();
        let reg = mock_reg::<MockResource, N>();
        let ctx = MockResource::mock_ctx();

        // alloc
        let (h, ctx) = reg.prepare(CONFIG, ctx).expect("alloc ok");
        assert!(!h.is_null());
        assert_eq!(reg.len(), 1);

        // acquire
        let (Some(g), ctx) = reg.view(h, ctx) else {
            panic!("acquire ok")
        };
        tracing::debug!("Acquired: {:?}", g);
        assert_eq!(g.view, CONFIG);
        drop(g);
        assert_eq!(reg.len(), 1);

        // free
        let ctx = reg.clear(h, ctx);
        assert_eq!(reg.len(), 0);
        assert_eq!(ctx.0.load(Ordering::Relaxed), 1);
        assert_eq!(ctx.1.load(Ordering::Relaxed), 1);

        let mut cur_ctx = ctx;
        let mut ids = Vec::new();
        for _ in 0..N {
            let (h, ctx) = reg
                .prepare(MockResource::mock_id(), cur_ctx)
                .expect("alloc ok");
            cur_ctx = ctx;
            ids.push(h);
        }

        let mut cur_ctx = reg
            .prepare(MockResource::mock_id(), cur_ctx)
            .expect_err("alloc failed");

        for id in ids {
            let g = Registry::acquire(&reg, id).expect("acquire ok");
            assert_eq!(EntryGuard::rc(&g), 1);
            tracing::debug!("Acquired: {:?}", g);

            let len = reg.len();

            let g2 = Registry::acquire(&reg, id).expect("acquire ok");
            assert_eq!(EntryGuard::rc(&g2), 2);
            tracing::debug!("Acquired 2: {:?}", g2);

            let ctx = reg.clear(id, cur_ctx);
            cur_ctx = ctx;
            let clear_failed_len = reg.len();
            assert_eq!(
                len, clear_failed_len,
                "Registry shouldn't clear resource if guard exists"
            );

            drop(g);
            drop(g2);
            let ctx = reg.clear(id, cur_ctx);
            cur_ctx = ctx;
            let clear_succ_len = reg.len();
            assert_ne!(
                len, clear_succ_len,
                "Registry should clear resource if guard doesn't exists"
            );
            tracing::debug!("Before length: {}, After Length: {}", len, clear_succ_len);
        }
    }

    #[test]
    fn multi_thread() {
        use crossbeam_utils::Backoff;
        use std::thread;

        const THREAD_NUM: usize = 8;
        const N: usize = 16;
        tracing_init();
        let reg = mock_reg::<MockResource, N>();
        let ctx = MockResource::mock_ctx();

        let ths: Vec<_> = (0..THREAD_NUM)
            .map(|_| {
                let reg = reg.clone();
                let ctx = ctx.clone();
                thread::spawn(move || {
                    let backoff = Backoff::new();
                    let mut cur_ctx = ctx;
                    for _ in 0..N / 2 {
                        let cfg = MockResource::mock_id();
                        let (h, ctx) = match reg.prepare(cfg, cur_ctx) {
                            Ok(res) => res,
                            Err(ctx) => {
                                backoff.snooze();
                                cur_ctx = ctx;
                                continue;
                            }
                        };
                        for _ in 0..N / 2 {
                            if let Some(g) = Registry::acquire(&reg, h) {
                                assert_eq!(g.as_ref().id, cfg);
                                tracing::debug!("Guard: {:?}", g);
                                // small work
                                core::hint::black_box(&*g);
                            }
                        }
                        cur_ctx = reg.clear(h, ctx);
                    }
                })
            })
            .collect();

        let _: Vec<_> = ths.into_iter().map(|t| t.join().unwrap()).collect();

        assert_eq!(reg.len(), 0);
        assert_eq!(ctx.0.load(Ordering::Relaxed), ctx.1.load(Ordering::Relaxed));
        tracing::debug!("Initiated: {}", ctx.0.load(Ordering::Relaxed));
    }

    #[test]
    fn concurrent_acquire() {
        use std::thread;

        const N: usize = 4;
        const THREAD_NUM: usize = 8;
        const ACQUIRE_NUM: usize = 500;

        tracing_init();
        let reg = mock_reg::<MockResource, N>();
        let ctx = MockResource::mock_ctx();

        let mut cur_ctx = ctx;
        for _ in 0..N {
            let (_, ctx) = reg
                .prepare(MockResource::mock_id(), cur_ctx)
                .expect("alloc ok");
            cur_ctx = ctx;
        }

        let threads: Vec<_> = (0..THREAD_NUM)
            .map(|_| {
                let reg = reg.clone();
                let h = reg
                    .lookup(fastrand::usize(0..N))
                    .expect("resource should exists");
                thread::spawn(move || {
                    for _ in 0..ACQUIRE_NUM {
                        if let Some(g) = Registry::acquire(&reg, h) {
                            // small work
                            tracing::debug!("Guard: {:?}", g);
                            core::hint::black_box(&*g);
                        }
                    }
                })
            })
            .collect();

        let _: Vec<_> = threads.into_iter().map(|th| th.join().unwrap()).collect();

        assert_eq!(reg.len(), N);

        for idx in 0..N {
            let h = reg.lookup(idx).expect("resource should exists");
            let ctx = reg.clear(h, cur_ctx);
            cur_ctx = ctx;
        }
        assert_eq!(
            cur_ctx.0.load(Ordering::Relaxed),
            cur_ctx.1.load(Ordering::Relaxed)
        )
    }

    #[test]
    fn aba_impede() {
        const N: usize = 1;

        tracing_init();
        let reg = mock_reg::<MockResource, N>();
        let ctx = MockResource::mock_ctx();

        let (h1, ctx) = reg.prepare(MockResource::mock_id(), ctx).expect("alloc ok");
        assert_eq!(reg.len(), 1);
        let ctx = reg.clear(h1, ctx);
        assert_eq!(reg.len(), 0);

        let (h2, ctx) = reg.prepare(MockResource::mock_id(), ctx).expect("alloc ok");
        assert_eq!(reg.len(), 1);

        // aba
        assert!(Registry::acquire(&reg, h1).is_none());
        assert!(Registry::acquire(&reg, h2).is_some());

        let _ = reg.clear(h2, ctx);
        assert_eq!(reg.len(), 0);
    }
}
