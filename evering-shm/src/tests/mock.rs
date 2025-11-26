#![cfg(test)]

use core::ops::{Deref, DerefMut};

use memory_addr::{MemoryAddr, VirtAddr};

use crate::area::{AddrSpec, MemBlkHandle, Mmap, Mprotect, RawMemBlk};
use crate::malloc::{MemAllocInfo, MemAllocator};
use crate::msg::MoveMessage;
use crate::tracing_init;
use crate::{ArenaMemBlk, Conn, Optimistic};

const MAX_ADDR: usize = 0x10000;

type MockFlags = u8;
type MockPageTable = [MockFlags; MAX_ADDR];
type MockMemHandle<'a> = MemBlkHandle<MockAddr, MockBackend<'a>>;
type MockArena<'a> = ArenaMemBlk<MockAddr, MockBackend<'a>, Optimistic>;
type MockConn<'a, const N: usize> = Conn<MockAddr, MockBackend<'a>, Optimistic, (), N>;

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
    type Config = ();
    type Error = ();

    fn map(
        self,
        start: Option<<MockAddr as AddrSpec>::Addr>,
        size: usize,
        flags: <MockAddr as AddrSpec>::Flags,
        _cfg: (),
    ) -> Result<RawMemBlk<MockAddr, Self>, Self::Error> {
        let start = match start {
            Some(start) => start,
            None => 0.into(),
        };
        for entry in self.0.iter_mut().skip(start.as_usize()).take(size) {
            if *entry != 0 {
                return Err(());
            }
            *entry = flags;
        }
        let start = self.start().add(start.as_usize());
        Ok(RawMemBlk::from_raw(start, size, flags, self))
    }

    fn unmap(area: &mut RawMemBlk<MockAddr, Self>) -> Result<(), Self::Error> {
        let start = area.a.start();
        let arr_start = area.bk.arr_addr(start);
        let size = area.a.size();
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
    fn protect(
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

fn mock_handle(bk: MockBackend<'_>, start: Option<VirtAddr>, size: usize) -> MockMemHandle<'_> {
    let a = MemBlkHandle::init(bk, start, size, 0, ()).unwrap();
    a.into()
}

fn mock_arena(bk: MockBackend<'_>, start: Option<VirtAddr>, size: usize) -> MockArena<'_> {
    let a = ArenaMemBlk::init(bk, start, size, 0, ()).unwrap();
    a
}

fn mock_conn<const N: usize>(
    bk: MockBackend<'_>,
    start: Option<VirtAddr>,
    size: usize,
) -> MockConn<'_, N> {
    let a = Conn::init(bk, start, size, 0, ()).unwrap();
    a
}

#[test]
fn area_init() {
    const STEP: usize = 0x2000;
    let mut pt = [0; MAX_ADDR];
    for start in (0..MAX_ADDR).step_by(STEP) {
        let bk = MockBackend(&mut pt);
        let a = mock_handle(bk, Some(start.into()), STEP);
        tracing::debug!("{}", a.header());
    }
}

#[test]
fn arena_exceed() {
    use std::sync::Barrier;
    use std::thread;

    use crate::malloc::MemAlloc;

    const BYTES_SIZE: usize = 50;
    const REDUCED_SIZE: usize = 35;
    const START: usize = 1;
    const NUM: usize = 5;

    tracing_init();
    let mut pt = [0; MAX_ADDR];
    let bk = MockBackend(&mut pt);
    let mem = mock_arena(bk, Some(0.into()), MAX_ADDR);
    let a = mem.arena();

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

    use crate::malloc::MemAlloc;

    const BYTES_SIZE: usize = 4;
    const ALLOC_NUM: usize = 1000;
    const NUM: usize = 10;

    tracing_init();
    let mut pt = [0; MAX_ADDR];
    let bk = MockBackend(&mut pt);
    let mem = mock_arena(bk, Some(0.into()), MAX_ADDR);
    let a = mem.arena();

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

    use crate::malloc::MemAlloc;

    const BYTES_SIZE: usize = 8;
    const ALLOC_NUM: usize = 1000;
    const NUM: usize = 5;

    tracing_init();
    let mut pt = [0; MAX_ADDR];
    let bk = MockBackend(&mut pt);
    let mem = mock_arena(bk, Some(0.into()), MAX_ADDR);
    let a = mem.arena();

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
    let bk = MockBackend(&mut pt);
    let mem = mock_arena(bk, Some(0.into()), MAX_ADDR);
    let a = mem.arena();

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
    const ALLOC_NUM: usize = 100;
    const NUM: usize = 5;

    let mut pt = [0; MAX_ADDR];
    let bk = MockBackend(&mut pt);
    let mem = mock_arena(bk, Some(0.into()), MAX_ADDR);
    let a = mem.arena();

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
    let mem = mock_arena(bk, Some(0.into()), MAX_ADDR);
    let a = mem.arena();
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
    let bk = MockBackend(&mut pt);
    let mem = mock_arena(bk, Some(0.into()), MAX_ADDR);
    let a = mem.arena();

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
fn conn() {
    use crate::msg::{Message, Move, TypeTag, type_id};

    const N: usize = 1;
    const SIZE: usize = 20;

    #[derive(Debug)]
    struct Info(u32);

    impl Info {
        fn mock() -> Self {
            Self(fastrand::u32(0..100))
        }
    }

    impl TypeTag for Info {
        const TYPE_ID: crate::msg::TypeId = type_id::type_id("My");
    }

    impl Message for Info {
        type Semantics = Move;
    }

    tracing_init();

    let mut pt = [0; MAX_ADDR];
    let bk = MockBackend(&mut pt);
    let conn = mock_conn::<N>(bk, Some(0.into()), MAX_ADDR);
    let alloc = conn.arena();

    let h = conn.prepare(SIZE).expect("alloc ok");
    let q = conn.acquire(h).expect("view ok");

    let (token_of, alloc) = Info::mock().token(alloc);
    let token = token_of.pack();
    let (ls, lr) = q.clone().sr_duplex();
    let (rs, rr) = q.rs_duplex();

    ls.try_send(token).expect("send ok");
    let t = rr.try_recv().expect("recv ok");
    let t = Info::detoken(t.into_parts().0, alloc).expect("detoken ok");
    tracing::debug!("{:?}", t);
}
