use crate::perlude::talc::MapAlloc;
use crate::tests::mock::{MAX_ADDR, MockAddr, MockBackend};
use crate::tests::{self};

// use crate::perlude::allocator::{MapAlloc, Optimistic};
// use crate::perlude::{Session, SessionBy};
//
type MockAlloc<'a> = MapAlloc<MockAddr, MockBackend<'a>>;

fn mock_alloc(bk: &mut [u8], start: usize, size: usize) -> MockAlloc<'_> {
    let bk = MockBackend(bk);
    bk.shared(start, size).try_into().unwrap()
}

#[test]
fn pbox_rand() {
    const ALLOC_NUM: usize = 500;
    const NUM: usize = 5;

    let mut pt = [0; MAX_ADDR];
    let a = mock_alloc(&mut pt, 0, MAX_ADDR);

    tests::pbox_rand::<ALLOC_NUM, NUM>(a);
}

#[test]
fn pbox_token() {
    const ALLOC_NUM: usize = 500;
    const NUM: usize = 5;

    let mut pt = [0; MAX_ADDR];
    let a = mock_alloc(&mut pt, 0, MAX_ADDR);

    tests::pbox_token::<ALLOC_NUM, NUM>(a);
}