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
    const ALLOC_NUM: usize = 1;
    const NUM: usize = 5;

    let mut pt = [0; MAX_ADDR];
    let a = mock_alloc(&mut pt, 0, MAX_ADDR);

    let a_ref = &a;
    let b = PBox::new_in(HighAlign(rand_num()), &a_ref);
    let ptr_addr = b.as_ptr().addr();

    let len = rand_len();
    let mut slice_b = PBox::new_slice_in(len, |_| rand_num(), &a_ref);
    let bar = Barrier::new(NUM);
    // thread::scope(|s| {
    //     for _ in 0..NUM {
    //         let a_ref = &a;
    //         let b_ref = &bar;

    //         s.spawn(move || {
    //             b_ref.wait();
    //             for _ in 0..ALLOC_NUM {
    //                 let b = PBox::new_in(HighAlign(rand_num()), &a_ref);
    //                 let ptr_addr = b.as_ptr().addr();

    //                 let len = rand_len();
    //                 let mut slice_b = PBox::new_slice_in(len, |_| rand_num(), &a_ref);

    //                 // Modification
    //                 const NULL: u64 = 0;
    //                 for i in slice_b.iter_mut() {
    //                     *i = NULL;
    //                 }

    //                 for i in slice_b.iter() {
    //                     assert_eq!(*i, NULL, "PBox modification failed");
    //                 }

    //                 tracing::debug!("Align Box: {:?}", &b);
    //                 tracing::debug!("Slice: {:?}", slice_b);
    //                 assert_eq!(ptr_addr % ALIGN, 0, "PBox allocation in wrong alignment");
    //                 assert_eq!(slice_b.len(), len, "PBox allocation in wrong length");
    //             }
    //         });
    //     }
    // });
}
