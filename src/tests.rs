#![cfg(test)]
#![allow(dead_code)]

use crate::{
    mem::{self, Map, MapView, MemAllocator, MemOps, TransferAllocator},
    msg::Repr,
};

mod mock;
#[cfg(all(feature = "map", unix))]
mod unix;

#[inline]
pub(crate) fn tracing_init() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::ERROR)
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

impl MemBlkTestIO for Map {
    #[inline]
    unsafe fn write_bytes(&self, data: &[u8], len: usize, offset: usize) {
        use crate::mem::Access;

        debug_assert!(self.size() >= data.len() + offset);
        debug_assert!(data.len() >= len);

        if self.permits(Access::WRITE).is_err() {
            panic!("[write]: permission denied")
        }
        unsafe {
            use crate::mem::MemOps;
            core::ptr::copy_nonoverlapping(data.as_ptr(), self.start_mut_ptr().add(offset), len)
        };
    }

    #[inline]
    unsafe fn read_bytes(&self, buf: &mut [u8], len: usize, offset: usize) {
        use crate::mem::Access;

        debug_assert!(self.size() >= buf.len() + offset);
        debug_assert!(buf.len() >= len);

        if self.permits(Access::READ).is_err() {
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

unsafe impl Repr for Info {
    const SCHEMA: crate::SchemaKey =
        crate::SchemaKey::new(crate::schema::schema_id("evering.test.info"), 1);
}

fn analyze_latencies(all: Vec<Vec<u64>>) {
    use hdrhistogram::Histogram;

    const BOUND: u64 = 10_000_000_000;
    let mut hist = Histogram::<u64>::new_with_bounds(1, BOUND, 3).unwrap();

    for thread_results in all {
        for latency in thread_results {
            // Record each operation's latency in nanoseconds
            hist.saturating_record(latency);
        }
    }

    println!("\n--- IPC Allocator Contention Report ---");
    println!("Total Operations: {}", hist.len());
    println!(
        "Throughput:       {:.2} ops/sec",
        (hist.len() as f64 / (hist.max() as f64 / 1e9))
    );

    println!("\nLatency Distribution:");
    println!("  Min:    {:>10} ns", hist.min());
    println!("  p50:    {:>10} ns (Median)", hist.value_at_quantile(0.5));
    println!("  p90:    {:>10} ns", hist.value_at_quantile(0.9));
    println!(
        "  p99:    {:>10} ns (The 'Stall' point)",
        hist.value_at_quantile(0.99)
    );
    println!("  p99.9:  {:>10} ns", hist.value_at_quantile(0.999));
    println!("  Max:    {:>10} ns", hist.max());

    // Visualizing the "Knee" of the curve
    if hist.value_at_quantile(0.99) > hist.value_at_quantile(0.5) * 10 {
        println!("\n[!] WARNING: High Tail Latency detected.");
        println!("    Your p99 is >10x your median. This suggests heavy lock contention");
        println!("    or 'stop-the-world' events in your allocator metadata management.");
    }
}

fn area_init(v: MapView) {
    tracing_init();

    tracing::debug!("area header: {:?}, {:?}", v.header(), v.header().status());
    tracing::debug!("[Area]: {:?}", v);
    tracing::debug!("[Area]: header: {:?}", v);
}

fn alloc_lines<const BYTES_SIZE: usize, const ALLOC_NUM: usize, const NUM: usize>(
    a: impl MemAllocator<Error = impl core::fmt::Debug, Meta = impl Send + core::fmt::Debug> + Sync,
) {
    use std::sync::Barrier;
    use std::thread;

    tracing_init();

    let bar = Barrier::new(NUM);
    let mut metas: Vec<_> = (0..ALLOC_NUM)
        .map(|_| a.alloc_bytes(BYTES_SIZE).unwrap())
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
                    let _ = a_ref.dealloc_bytes(meta);
                }
            });
        }
    });
}

fn alloc_content<const BYTES_SIZE: usize, const OPS_PER_THREAD: usize, const NUM: usize>(
    a: impl MemAllocator<Error = impl core::fmt::Debug, Meta = impl core::fmt::Debug> + Sync + Send,
) {
    use alloc::sync::Arc;
    use std::sync::Barrier;
    use std::thread;
    use std::time::Instant;

    const BOUND: usize = 10;

    let a = Arc::new(a);
    let bar = Arc::new(Barrier::new(NUM));

    let results = thread::scope(|s| {
        let mut handlers = Vec::new();

        for _ in 0..NUM {
            let a_ref = Arc::clone(&a);
            let bar_ref = Arc::clone(&bar);

            let h = s.spawn(move || {
                let mut latencies = Vec::with_capacity(OPS_PER_THREAD);
                let mut active_allocs = std::collections::VecDeque::new();

                bar_ref.wait(); // Global Sync Start

                for _ in 0..OPS_PER_THREAD {
                    let op_start = Instant::now();

                    // 1. Stress the allocator: Mix Malloc and Free
                    if let Ok(meta) = a_ref.alloc_bytes(BYTES_SIZE) {
                        active_allocs.push_back(meta);
                    }

                    // Maintain a "window" of outstanding allocations
                    // to simulate high-pressure shared memory usage
                    if active_allocs.len() > BOUND
                        && let Some(old_meta) = active_allocs.pop_front()
                    {
                        // Explicitly drop/deallocate here
                        let _ = a_ref.dealloc_bytes(old_meta);
                    }

                    latencies.push(op_start.elapsed().as_nanos() as u64);
                }
                latencies
            });
            handlers.push(h);
        }

        handlers
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Vec<_>>()
    });
    analyze_latencies(results);
}

fn pbox_droppy<const ALLOC_NUM: usize, const NUM: usize>(
    a: impl MemAllocator<Error = impl core::fmt::Debug> + Sync,
) {
    use std::sync::Arc;
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    use crate::boxed::PBoxIn;

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
                    let slot = loop {
                        match PBoxIn::try_new_uninit_in(a_ref) {
                            Ok(slot) => break slot,
                            Err(_) => thread::yield_now(),
                        }
                    };
                    let droppy = slot.write(Droppy(c_ref.clone()));
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

    use crate::boxed::PBoxIn;

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
                    let b = loop {
                        match PBoxIn::try_new_in(HighAlign(rand_num()), &a_ref) {
                            Ok(value) => break value,
                            Err(_) => thread::yield_now(),
                        }
                    };
                    let ptr_addr = b.as_ptr().addr();

                    let len = rand_len();
                    let mut slice_b = loop {
                        match PBoxIn::try_new_slice_in(len, |_| rand_num(), &a_ref) {
                            Ok(value) => break value,
                            Err(_) => thread::yield_now(),
                        }
                    };

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

fn pbox_token<const ALLOC_NUM: usize, const NUM: usize>(
    a: impl TransferAllocator<Error = impl core::fmt::Debug, Meta = impl Send + mem::Meta> + Sync,
) {
    use std::sync::Barrier;
    use std::thread;

    use crate::boxed::PBoxIn;

    #[derive(Debug)]
    struct Recover {
        f1: u64,
        f2: char,
    }

    impl Recover {
        fn rand() -> Self {
            Self {
                f1: fastrand::u64(0..100),
                f2: fastrand::char('a'..='z'),
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
                            let recover = loop {
                                match PBoxIn::try_new_in(Recover::rand(), &a_ref) {
                                    Ok(recover) => break recover,
                                    Err(_) => std::thread::yield_now(),
                                }
                            };
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
