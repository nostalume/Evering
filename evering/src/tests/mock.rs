#![cfg(test)]

use core::ops::{Deref, DerefMut};

use memory_addr::{MemoryAddr, VirtAddr};

use crate::mem::{AddrSpec, MemBlkHandle, MemBlkLayout, Mmap, Mprotect, RawMemBlk};
use crate::mem::{MemAllocInfo, MemAllocator};
use crate::msg::Envelope;
use crate::perlude::{
    AConn, Conn,
    allocator::{Config, MemArena, Optimistic},
};

use crate::tests::{prob, tracing_init};

const MAX_ADDR: usize = 0x10000;

type MockFlags = u8;
type MockPageTable = [MockFlags];

struct MockAddr;

impl AddrSpec for MockAddr {
    type Addr = VirtAddr;
    type Flags = MockFlags;
}

struct MockBackend<'a>(&'a mut MockPageTable);

impl<'a> Deref for MockBackend<'a> {
    type Target = MockPageTable;

    fn deref(&self) -> &Self::Target {
        self.0
    }
}

impl<'a> DerefMut for MockBackend<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0
    }
}

impl MockBackend<'_> {
    fn start(&self) -> VirtAddr {
        self.0.as_ptr().addr().into()
    }

    fn arr_addr(&self, addr: VirtAddr) -> usize {
        // addr - self.start
        addr.sub_addr(self.start())
    }
}

impl<'a> Mmap<MockAddr> for MockBackend<'a> {
    // Due to mock addr, start is not real addr.
    // We take handle as offset from ptr of array.
    type Handle = usize;
    type MapFlags = ();
    type Error = ();

    fn map(
        self,
        _start: Option<<MockAddr as AddrSpec>::Addr>,
        size: usize,
        _mflags: (),
        pflags: <MockAddr as AddrSpec>::Flags,
        handle: usize,
    ) -> Result<RawMemBlk<MockAddr, Self>, Self::Error> {
        for entry in self.0.iter_mut().skip(handle).take(size) {
            if *entry != 0 {
                return Err(());
            }
            *entry = pflags;
        }
        let start = self.start().add(handle);
        Ok(unsafe { RawMemBlk::from_raw(start, size, pflags, self) })
    }

    fn unmap(area: &mut RawMemBlk<MockAddr, Self>) -> Result<(), Self::Error> {
        let start = area.spec.start();
        let arr_start = area.bk.arr_addr(start);
        let size = area.spec.size();
        for entry in area.bk.iter_mut().skip(arr_start).take(size) {
            if *entry == 0 {
                return Err(());
            }
            *entry = 0;
        }
        Ok(())
    }
}

impl<'a> Mprotect<MockAddr> for MockBackend<'a> {
    unsafe fn protect(
        area: &mut RawMemBlk<MockAddr, Self>,
        new_flags: <MockAddr as AddrSpec>::Flags,
    ) -> Result<(), Self::Error> {
        let start = area.start();
        let arr_start = area.bk.arr_addr(start);
        let size = area.size();
        for entry in area.bk.iter_mut().skip(arr_start).take(size) {
            if *entry == 0 {
                return Err(());
            }
            *entry = new_flags;
        }
        Ok(())
    }
}

impl MockBackend<'_> {
    fn shared(self, start: usize, size: usize) -> MemBlkLayout<MockAddr, Self> {
        MemBlkLayout::new(self.map(None, size, (), 0, start).unwrap()).unwrap()
    }
}

type MockMemHandle<'a> = MemBlkHandle<MockAddr, MockBackend<'a>>;
type MockArena<'a> = MemArena<Optimistic, MockAddr, MockBackend<'a>>;
type MockConn<'a, H, const N: usize> = Conn<MockAddr, MockBackend<'a>, Optimistic, H, N>;
type MockAConn<'a, H, const N: usize> = AConn<MockAddr, MockBackend<'a>, Optimistic, H, N>;

fn mock_handle(bk: &mut [u8], start: usize, size: usize) -> MockMemHandle<'_> {
    let bk = MockBackend(bk);
    bk.shared(start, size).try_into().unwrap()
}

fn mock_arena(bk: &mut [u8], start: usize, size: usize) -> MockArena<'_> {
    let bk = MockBackend(bk);
    MockArena::from_layout(bk.shared(start, size), Config::default()).unwrap()
}

fn mock_conn<H: Envelope, const N: usize>(
    bk: &mut [u8],
    start: usize,
    size: usize,
) -> MockConn<'_, H, N> {
    let bk = MockBackend(bk);
    bk.shared(start, size).try_into().unwrap()
}

fn mock_aconn<H: Envelope, const N: usize>(
    bk: &mut [u8],
    start: usize,
    size: usize,
) -> MockAConn<'_, H, N> {
    let bk = MockBackend(bk);
    bk.shared(start, size).try_into().unwrap()
}

#[test]
fn area_init() {
    const STEP: usize = 0x2000;
    let mut pt = [0; MAX_ADDR];
    for start in (0..MAX_ADDR).step_by(STEP) {
        let a = mock_handle(&mut pt, start, STEP);
        tracing::debug!("{:?}", a.header());
    }
}

#[test]
fn arena_exceed() {
    use std::sync::Barrier;
    use std::thread;

    use crate::mem::MemAlloc;

    const BYTES_SIZE: usize = 50;
    const REDUCED_SIZE: usize = 35;
    const START: usize = 1;
    const NUM: usize = 5;

    tracing_init();
    let mut pt = [0; MAX_ADDR];
    let a = mock_arena(&mut pt, 0, MAX_ADDR);

    let bar = Barrier::new(NUM);
    let mut metas = Vec::new();

    for _ in START..=NUM {
        let bytes = a.malloc_bytes(BYTES_SIZE).unwrap();
        metas.push(bytes);
    }

    // Fill the header reserved bytes
    let remained = a.remained();
    let _ = a.malloc_bytes(remained).unwrap();
    // Now it generate freelist nodes
    metas.drain(..).for_each(|meta| {
        tracing::debug!("drain bytes: {:?}", meta);
        a.dealloc(meta);
    });

    thread::scope(|s| {
        for _ in (START..=NUM).rev() {
            let a = &a;
            let bar = &bar;

            s.spawn(move || {
                bar.wait();
                let meta = a.malloc_bytes(REDUCED_SIZE).unwrap();
                tracing::debug!("freelist bytes: {:?}", meta)
            });
        }
    });
}

#[test]
fn arena_frag() {
    use std::sync::Barrier;
    use std::thread;

    use crate::mem::MemAlloc;

    const BYTES_SIZE: usize = 4;
    const ALLOC_NUM: usize = 1000;
    const NUM: usize = 10;

    tracing_init();
    let mut pt = [0; MAX_ADDR];
    let a = mock_arena(&mut pt, 0, MAX_ADDR);

    let bar = Barrier::new(NUM);
    thread::scope(|s| {
        for _ in 0..NUM {
            let a_ref = &a;
            let b_ref = &bar;
            s.spawn(move || {
                b_ref.wait();

                for _ in 0..ALLOC_NUM {
                    if let Ok(meta) = a_ref.malloc_bytes(BYTES_SIZE) {
                        tracing::debug!("{:?}", meta);
                    }
                }
            });
        }
    });
}

#[test]
fn arena_dealloc() {
    use std::sync::Barrier;
    use std::thread;

    use crate::mem::MemAlloc;

    const BYTES_SIZE: usize = 8;
    const ALLOC_NUM: usize = 1000;
    const NUM: usize = 5;

    tracing_init();
    let mut pt = [0; MAX_ADDR];
    let a = mock_arena(&mut pt, 0, MAX_ADDR);

    let bar = Barrier::new(NUM);
    let mut metas: Vec<_> = (0..ALLOC_NUM)
        .map(|_| a.malloc_bytes(BYTES_SIZE).unwrap())
        .collect();
    thread::scope(|s| {
        for i in 0..NUM {
            let a_ref = &a;
            let b_ref = &bar;
            let start = 0;
            let end = if i == NUM - 1 {
                metas.len()
            } else {
                ALLOC_NUM / NUM
            };

            let chunk: Vec<_> = metas.drain(start..end).collect();
            s.spawn(move || {
                b_ref.wait();
                for meta in chunk {
                    tracing::debug!("{:?}", meta);
                    a_ref.dealloc(meta);
                }
            });
        }
    });
}

#[test]
fn pbox_droppy() {
    use std::sync::Arc;
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    use crate::boxed::PBox;

    tracing_init();
    // It should be clarified that
    // to share process-invariant data
    // the heap allocation context is not allowed
    // It's only test-oriented.
    let counter = Arc::new(AtomicUsize::new(0));
    struct Droppy(Arc<AtomicUsize>);
    impl Drop for Droppy {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    const ALLOC_NUM: usize = 1000;
    const NUM: usize = 5;

    let mut pt = [0; MAX_ADDR];
    let a = mock_arena(&mut pt, 0, MAX_ADDR);

    let bar = Barrier::new(NUM);
    thread::scope(|s| {
        for _ in 0..NUM {
            let c_ref = counter.clone();
            let a_ref = &a;
            let b_ref = &bar;

            s.spawn(move || {
                b_ref.wait();
                for _ in 0..ALLOC_NUM {
                    let droppy = PBox::new_in(Droppy(c_ref.clone()), a_ref.clone());
                    tracing::debug!("counter: {:?}", droppy.0.load(Ordering::Relaxed));
                    drop(droppy)
                    // drop(droppy)
                }
            });
        }
    });

    assert_eq!(
        counter.load(Ordering::Relaxed),
        NUM * ALLOC_NUM,
        "Counter must have been called on drop"
    );
}

#[test]
fn pbox_dyn() {
    use std::sync::Barrier;
    use std::thread;

    use crate::boxed::PBox;

    tracing_init();
    #[derive(Debug)]
    #[repr(C, align(64))]
    struct HighAlign(u64);
    const ALIGN: usize = core::mem::align_of::<HighAlign>();

    fn rand_num() -> u64 {
        const HRANGE: u64 = 500;
        fastrand::u64(0..HRANGE)
    }

    fn rand_len() -> usize {
        const SRANGE: usize = 20;
        fastrand::usize(0..SRANGE)
    }

    // Choose a smaller number due to large allocation.
    const ALLOC_NUM: usize = 50;
    const NUM: usize = 5;

    let mut pt = [0; MAX_ADDR];
    let bk = MockBackend(&mut pt);
    let a = mock_arena(&mut pt, 0, MAX_ADDR);

    let bar = Barrier::new(NUM);
    thread::scope(|s| {
        for _ in 0..NUM {
            let a_ref = &a;
            let b_ref = &bar;

            s.spawn(move || {
                b_ref.wait();
                for _ in 0..ALLOC_NUM {
                    let b = PBox::new_in(HighAlign(rand_num()), &a_ref);
                    let ptr_addr = b.as_ptr().addr();

                    let len = rand_len();
                    let mut slice_b = PBox::new_slice_in(len, |_| rand_num(), &a_ref);

                    // Modification
                    const NULL: u64 = 0;
                    for i in slice_b.iter_mut() {
                        *i = NULL;
                    }

                    for i in slice_b.iter() {
                        assert_eq!(*i, NULL, "PBox modification failed");
                    }

                    tracing::debug!("Align Box: {:?}", &b);
                    tracing::debug!("Slice: {:?}", slice_b);
                    assert_eq!(ptr_addr % ALIGN, 0, "PBox allocation in wrong alignment");
                    assert_eq!(slice_b.len(), len, "PBox allocation in wrong length");
                }
            });
        }
    });
}

#[test]
fn parc_stress() {
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    use crate::boxed::PArc;

    tracing_init();
    #[derive(Clone)]
    struct Droppy<A: MemAllocator>(PArc<AtomicUsize, A>);
    impl<A: MemAllocator> Drop for Droppy<A> {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    const CLONE_NUM: usize = 1000;
    const NUM: usize = 5;

    let mut pt = [0; MAX_ADDR];
    let bk = MockBackend(&mut pt);
    let a = mock_arena(&mut pt, 0, MAX_ADDR);
    let droppy = Droppy(PArc::new_in(AtomicUsize::new(0), &a));

    let bar = Barrier::new(NUM);
    thread::scope(|s| {
        for _ in 0..NUM {
            let b_ref = &bar;

            s.spawn(|| {
                b_ref.wait();
                // clone and drop
                let _: Vec<_> = (0..CLONE_NUM).map(|_| droppy.clone()).collect();
            });
        }
    });

    assert_eq!(
        droppy.0.load(Ordering::Relaxed),
        NUM * CLONE_NUM,
        "Counter must have been called on drop"
    );
}

#[test]
fn token_of_pbox() {
    use std::sync::Barrier;
    use std::thread;

    use crate::boxed::PBox;

    tracing_init();
    #[derive(Debug)]
    struct Recover {
        f1: u64,
        f2: char,
    }

    impl Recover {
        fn rand() -> Self {
            Self {
                f1: fastrand::u64(0..100),
                f2: fastrand::char('a'..'z'),
            }
        }
    }

    const ALLOC_NUM: usize = 500;
    const NUM: usize = 5;

    let mut pt = [0; MAX_ADDR];
    let a = mock_arena(&mut pt, 0, MAX_ADDR);

    let bar = Barrier::new(NUM);
    thread::scope(|s| {
        let handles = (0..NUM)
            .map(|_| {
                let a_ref = &a;
                let b_ref = &bar;

                s.spawn(move || {
                    b_ref.wait();
                    (0..ALLOC_NUM)
                        .map(move |_| {
                            let recover = PBox::new_in(Recover::rand(), &a_ref);
                            recover.token_of()
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>();

        let tokens: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        let _: Vec<_> = tokens
            .into_iter()
            .map(|chunk| {
                let a_ref = &a;
                let b_ref = &bar;

                s.spawn(move || {
                    b_ref.wait();
                    chunk.into_iter().for_each(|token| {
                        let recover = token.detoken(&a_ref);
                        tracing::debug!("{:?}", recover)
                    })
                })
            })
            .collect();
    });
}

#[test]
fn conn_sync() {
    use std::thread;

    use super::{Exit, Info};
    use crate::msg::MoveMessage;
    use crate::perlude::channel::{MsgReceiver, MsgSender, Token};

    const N: usize = 1;
    const SIZE: usize = 256;
    const FUZZ_PROB: f32 = 0.001;

    fn stress<S: MsgSender<Exit>, R: MsgReceiver<Exit>, F: FnMut(Token) -> Option<Token>>(
        s: S,
        r: R,
        mut handler: F,
    ) {
        let mut alive = true;

        while alive {
            if !prob(FUZZ_PROB) {
                thread::yield_now();
            }

            let Ok(p) = r.try_recv() else {
                continue;
            };

            if p.tag::<Exit>() == Exit::Exit {
                break;
            }

            let (t, h) = p.into_parts();
            assert!(h == Exit::None, "header corrupted");

            match handler(t) {
                None => {
                    let exit = Token::empty().pack(Exit::Exit);
                    let _ = s.try_send(exit);
                    alive = false;
                }
                Some(reply) => {
                    let pack = reply.pack(Exit::None);
                    let _ = s.try_send(pack);
                }
            }
        }
    }

    tracing_init();

    let mut pt = [0; MAX_ADDR];
    let conn = mock_conn::<Exit, N>(&mut pt, 0, MAX_ADDR);
    let alloc = conn.arena_ref();

    let h = conn.prepare(SIZE).expect("alloc ok");
    let q = conn.acquire(h).expect("view ok");

    let (ls, lr) = q.clone().sr_duplex();
    let (rs, rr) = q.clone().rs_duplex();

    let (msg, alloc) = Info::mock().token(alloc);
    let _ = ls.try_send(msg.pack(Exit::None));

    let handler = |token: Token, label: &'static str| {
        if prob(FUZZ_PROB) {
            None
        } else {
            let info = Info::detoken(token, &alloc).expect("should work");
            tracing::debug!("[{}] receive: {:?}", label, info);
            let (new, _) = Info::mock().token(&alloc);
            Some(new)
        }
    };
    thread::scope(|s| {
        s.spawn(|| stress(ls, lr, |token| handler(token, "Left")));

        s.spawn(|| {
            stress(rs, rr, |token| handler(token, "right"));
        });
    });
}

#[tokio::test(flavor = "current_thread")]
async fn conn_async() {
    use super::{Exit, IdExit, Info};
    use crate::msg::{MoveMessage, Tag};
    use crate::perlude::allocator::MemAllocator;
    use crate::perlude::channel::{
        CachePool, MsgCompleter, MsgReceiver, MsgSender, MsgSubmitter, Token,
    };

    const N: usize = 1;
    const SIZE: usize = 256;
    const FUZZ_PROB: f32 = 0.001;

    async fn req<S: MsgSubmitter<Exit>, A: MemAllocator>(s: S, alloc: A) {
        loop {
            tokio::task::yield_now().await;

            let (msg, _) = Info::mock().token(&alloc);
            let token = if prob(FUZZ_PROB) {
                msg.pack(Exit::Exit)
            } else {
                msg.pack(Exit::None)
            };

            let op = match s.try_submit(token) {
                Some(op) => op,
                None => continue,
            };
            let res = op.await;
            let (res, h) = res.into_parts();
            if h.tag() == Exit::Exit {
                break;
            }
            let info = Info::detoken(res, &alloc).unwrap();
            tracing::debug!("res: {:?}", info);
            drop(info)
        }
    }

    async fn complete<R: MsgCompleter<Exit>>(r: R) {
        loop {
            tokio::task::yield_now().await;
            if let Some(false) = r.try_complete(|token| token.tag::<Exit>() != Exit::Exit) {
                tracing::debug!("drop complete");
                break;
            }
        }
    }

    async fn stress<
        S: MsgSender<IdExit>,
        R: MsgReceiver<IdExit>,
        F: FnMut(Token) -> Option<Token>,
    >(
        s: S,
        r: R,
        mut handler: F,
    ) {
        let mut alive = true;

        while alive {
            if !prob(FUZZ_PROB) {
                tokio::task::yield_now().await;
            }
            let Ok(p) = r.try_recv() else {
                continue;
            };

            if p.tag::<Exit>() == Exit::Exit {
                break;
            }

            let (t, h) = p.into_parts();
            assert!(h.tag() != Exit::Exit, "header corrupted");

            match handler(t) {
                None => {
                    let exit = Token::empty().pack(h.with_tag(Exit::Exit));
                    let _ = s.try_send(exit);
                    alive = false;
                }
                Some(reply) => {
                    let pack = reply.pack(h.with_tag(Exit::None));
                    let _ = s.try_send(pack);
                }
            }
        }
    }

    tracing_init();

    let mut pt = [0; MAX_ADDR];
    let conn = mock_aconn::<Exit, N>(&mut pt, 0, MAX_ADDR);
    let alloc = conn.arena();

    let h = conn.prepare(SIZE).expect("alloc ok");
    let q = conn.acquire(h).expect("view ok");

    let (ls, lr) = q.clone().sr_duplex();
    let (rs, rr) = q.clone().rs_duplex();

    let (ls, lr) = CachePool::<Exit, SIZE>::new().bind(ls, lr);
    //
    let alloc2 = alloc.clone();
    let handler = move |token: Token, label: &'static str| {
        if prob(FUZZ_PROB) {
            None
        } else {
            let info = Info::detoken(token, &alloc2).expect("should work");
            tracing::debug!("[{}] receive: {:?}", label, info);
            let (new, _) = Info::mock().token(&alloc2);
            Some(new)
        }
    };

    let stress = stress(rs, rr, move |token| handler(token, "Right"));
    let client = req(ls, &alloc);
    let complete = complete(lr);
    let _ = tokio::join!(stress, client, complete);
}
