use crate::mem::{MapView, MemAllocInfo, MemAllocator};
use crate::msg::Envelope;
use crate::perlude::talc::MapAlloc;
use crate::tests::mock::{MAX_ADDR, MockAddr, MockBackend};
use crate::tests::tracing_init;

// use crate::perlude::allocator::{MapAlloc, Optimistic};
// use crate::perlude::{Session, SessionBy};
//
type MockAlloc<'a> = MapAlloc<2, 256, 2, MockAddr, MockBackend<'a>>;

fn mock_alloc(bk: &mut [u8], start: usize, size: usize) -> MockAlloc<'_> {
    let bk = MockBackend(bk);
    bk.shared(start, size).try_into().unwrap()
}

#[test]
fn talc_exceed_box() {
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
    let a = mock_alloc(&mut pt, 0, MAX_ADDR);

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
    let a = mock_alloc(&mut pt, 0, MAX_ADDR);
    let droppy = Droppy(PArc::new_in(AtomicUsize::new(0), &a));
    
    let bar = Barrier::new(NUM);
    tracing::debug!("bar addr: {:?}",&raw const bar);
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

// #[test]
// fn token_of_pbox() {
//     use std::sync::Barrier;
//     use std::thread;

//     use crate::boxed::PBox;

//     #[derive(Debug)]
//     struct Recover {
//         f1: u64,
//         f2: char,
//     }

//     impl Recover {
//         fn rand() -> Self {
//             Self {
//                 f1: fastrand::u64(0..100),
//                 f2: fastrand::char('a'..'z'),
//             }
//         }
//     }

//     const ALLOC_NUM: usize = 500;
//     const NUM: usize = 5;

//     tracing_init();

//     let mut pt = [0; MAX_ADDR];
//     let a = mock_alloc(&mut pt, 0, MAX_ADDR);

//     let bar = Barrier::new(NUM);
//     thread::scope(|s| {
//         let handles = (0..NUM)
//             .map(|_| {
//                 let a_ref = &a;
//                 let b_ref = &bar;

//                 s.spawn(move || {
//                     b_ref.wait();
//                     (0..ALLOC_NUM)
//                         .map(move |_| {
//                             let recover = PBox::new_in(Recover::rand(), &a_ref);
//                             recover.token_of()
//                         })
//                         .collect::<Vec<_>>()
//                 })
//             })
//             .collect::<Vec<_>>();

//         let tokens: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

//         let _: Vec<_> = tokens
//             .into_iter()
//             .map(|chunk| {
//                 let a_ref = &a;
//                 let b_ref = &bar;

//                 s.spawn(move || {
//                     b_ref.wait();
//                     chunk.into_iter().for_each(|token| {
//                         let recover = token.detoken(&a_ref);
//                         tracing::debug!("{:?}", recover)
//                     })
//                 })
//             })
//             .collect();
//     });
// }