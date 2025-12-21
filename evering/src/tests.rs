#![cfg(test)]

use crate::{
    boxed::PBox,
    channel,
    mem::{self, AddrSpec, MapView, MemAllocInfo, MemAllocator, Mmap, RawMap},
    msg::{Envelope, Message, Move, TypeTag, type_id},
    token,
};

mod mock;
mod unix;

#[inline]
pub(crate) fn tracing_init() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .try_init();
}

#[inline]
pub(crate) fn prob(prob: f32) -> bool {
    fastrand::f32() < prob
}

pub(crate) trait MemBlkTestIO {
    unsafe fn write_bytes(&self, data: &[u8], len: usize, offset: usize);
    unsafe fn read_bytes(&self, buf: &mut [u8], len: usize, offset: usize);

    unsafe fn write_in(&self, data: &[u8], offset: usize) {
        unsafe { self.write_bytes(data, data.len(), offset) };
    }

    unsafe fn read_in(&self, len: usize, offset: usize) -> Vec<u8> {
        let mut buf = vec![0; len];
        unsafe { self.read_bytes(&mut buf, len, offset) };
        buf
    }

    unsafe fn write(&self, data: &[u8]) {
        unsafe { self.write_bytes(data, data.len(), 0) };
    }

    unsafe fn read(&self, len: usize) -> Vec<u8> {
        let mut buf = vec![0; len];
        unsafe { self.read_bytes(&mut buf, len, 0) };
        buf
    }
}

impl<S: AddrSpec, M: Mmap<S>> MemBlkTestIO for RawMap<S, M> {
    #[inline]
    unsafe fn write_bytes(&self, data: &[u8], len: usize, offset: usize) {
        use crate::mem::{Access, Accessible};

        debug_assert!(self.size() >= data.len() + offset);
        debug_assert!(data.len() >= len);

        if !self.spec.flags().permits(Access::WRITE) {
            panic!("[write]: permission denied")
        }
        unsafe {
            use crate::mem::MemOps;
            core::ptr::copy_nonoverlapping(data.as_ptr(), self.start_mut_ptr().add(offset), len)
        };
    }

    #[inline]
    unsafe fn read_bytes(&self, buf: &mut [u8], len: usize, offset: usize) {
        use crate::mem::{Access, Accessible};

        debug_assert!(self.size() >= buf.len() + offset);
        debug_assert!(buf.len() >= len);

        if !self.spec.flags().permits(Access::READ) {
            panic!("[read]: permission denied")
        }
        unsafe {
            use crate::mem::MemOps;
            core::ptr::copy_nonoverlapping(self.start_ptr().add(offset), buf.as_mut_ptr(), len)
        };
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Info {
    version: u32,
    data: u32,
}

impl Info {
    #[inline]
    pub fn mock() -> Self {
        Self {
            version: fastrand::u32(0..100),
            data: fastrand::u32(0..100),
        }
    }
}

impl TypeTag for Info {
    const TYPE_ID: crate::msg::TypeId = type_id::type_id("Info");
}

impl Message for Info {
    type Semantics = Move;
}

pub(crate) struct Infos<A: MemAllocator> {
    version: u32,
    data: token::TokenOf<[u8], A::Meta>,
}

impl<A: MemAllocator> Infos<A> {
    #[inline]
    pub fn mock(a: A) -> Self {
        Self {
            version: fastrand::u32(0..100),
            data: PBox::new_slice_in(fastrand::usize(0..128), |_| fastrand::u8(0..128), a)
                .token_of(),
        }
    }

    #[inline]
    pub fn data(&self, alloc: &A) -> &[u8] {
        unsafe { self.data.as_ptr(alloc).as_ref() }
    }
}

impl<A: MemAllocator> TypeTag for Infos<A> {
    const TYPE_ID: crate::msg::TypeId = type_id::type_id("Info");
}

impl<A: MemAllocator> Message for Infos<A> {
    type Semantics = Move;
}

fn area_init<S: AddrSpec, M: Mmap<S>>(v: MapView<S, M>) {
    tracing_init();

    tracing::debug!("area header: {:?}, {:?}", v.header(), v.header().status());
    tracing::debug!("[Area]: {:?}", v);
    let v2 = v.clone();
    tracing::debug!("[Area]: header: {:?}", v2);
}

fn alloc_exceed<const BYTES_SIZE: usize, const NUM: usize>(
    a: impl MemAllocator<Error = impl core::fmt::Debug, Meta = impl core::fmt::Debug>
    + MemAllocInfo
    + Clone
    + Sync,
) {
    use std::sync::Barrier;
    use std::thread;

    let reduced_size = BYTES_SIZE - 15;

    tracing_init();

    let bar = Barrier::new(NUM);
    let mut metas = Vec::new();

    for _ in 0..NUM {
        let bytes = a.malloc_bytes(BYTES_SIZE).unwrap();
        metas.push(bytes);
    }

    // Fill the header reserved bytes
    let remained = a.remained();
    let _ = a.malloc_bytes(remained).unwrap();
    // Now it generate freelist nodes
    metas.drain(..).for_each(|meta| {
        tracing::debug!("drain bytes: {:?}", meta);
        a.demalloc_bytes(meta);
    });

    thread::scope(|s| {
        for _ in 0..NUM {
            let a = &a;
            let bar = &bar;

            s.spawn(move || {
                bar.wait();
                match a.malloc_bytes(reduced_size) {
                    Ok(meta) => {
                        tracing::debug!("alloc bytes in node: {:?}", meta)
                    }
                    Err(e) => {
                        tracing::debug!("alloc bytes error in node: {:?}", e)
                    }
                }
            });
        }
    });
}

fn alloc_frag<const BYTES_SIZE: usize, const ALLOC_NUM: usize, const NUM: usize>(
    a: impl MemAllocator<Error = impl core::fmt::Debug, Meta = impl core::fmt::Debug> + Sync,
) {
    use std::sync::Barrier;
    use std::thread;

    tracing_init();

    let bar = Barrier::new(NUM);
    thread::scope(|s| {
        for _ in 0..NUM {
            let a_ref = &a;
            let bar = &bar;

            s.spawn(move || {
                bar.wait();
                for _ in 0..ALLOC_NUM {
                    if let Ok(meta) = a_ref.malloc_bytes(BYTES_SIZE) {
                        tracing::debug!("{:?}", meta);
                    }
                }
            });
        }
    });
}

fn alloc_dealloc<const BYTES_SIZE: usize, const ALLOC_NUM: usize, const NUM: usize>(
    a: impl MemAllocator<Error = impl core::fmt::Debug, Meta = impl Send + core::fmt::Debug> + Sync,
) {
    use std::sync::Barrier;
    use std::thread;

    tracing_init();

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
                    a_ref.demalloc_bytes(meta);
                }
            });
        }
    });
}

fn pbox_droppy<const ALLOC_NUM: usize, const NUM: usize>(
    a: impl MemAllocator<Error = impl core::fmt::Debug> + Sync,
) {
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

/// Choose a smaller number due to large allocation.
fn pbox_rand<const ALLOC_NUM: usize, const NUM: usize>(
    a: impl MemAllocator<Error = impl core::fmt::Debug> + Sync,
) {
    use std::sync::Barrier;
    use std::thread;

    use crate::boxed::PBox;

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

    tracing_init();

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

fn parc_stress<const CLONE_NUM: usize, const NUM: usize>(
    a: impl MemAllocator<Error = impl core::fmt::Debug> + Sync,
) {
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    use crate::boxed::PArc;

    #[derive(Clone)]
    struct Droppy<A: MemAllocator>(PArc<AtomicUsize, A>);
    impl<A: MemAllocator> Drop for Droppy<A> {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    tracing_init();

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

fn pbox_token<const ALLOC_NUM: usize, const NUM: usize>(
    a: impl MemAllocator<Error = impl core::fmt::Debug, Meta = impl Send + mem::Meta> + Sync,
) {
    use std::sync::Barrier;
    use std::thread;

    use crate::boxed::PBox;

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

    tracing_init();

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
                        let recover = token.boxed(&a_ref);
                        tracing::debug!("{:?}", recover)
                    })
                })
            })
            .collect();
    });
}

fn sync_stress<H: Envelope, M: mem::Meta, F: FnMut(token::Token<M>) -> Option<token::Token<M>>>(
    s: impl channel::Sender<
        Item = token::PackToken<H, M>,
        TryError = channel::TrySendError<token::PackToken<H, M>>,
    > + channel::QueueChannel,
    r: impl channel::Receiver<Item = token::PackToken<H, M>, TryError = channel::TryRecvError>
    + channel::QueueChannel,
    fuzz_prob: f32,
    mut handler: F,
) {
    use std::thread;

    loop {
        if !prob(fuzz_prob) {
            thread::yield_now();
        }

        let p = match r.try_recv() {
            Ok(p) => p,
            Err(e) => match e {
                channel::TryRecvError::Empty => continue,
                channel::TryRecvError::Disconnected => break,
            },
        };

        let (t, header) = p.unpack();

        match handler(t) {
            None => {
                s.close();
                break;
            }
            Some(reply) => {
                let pack = reply.with(header);
                let _ = s.try_send(pack);
            }
        }
    }
}
