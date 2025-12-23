use crate::msg::{Envelope, MoveMsg};
use crate::os::FdBackend;
use crate::os::unix::{AddrSpec, UnixFd};

use crate::perlude::talc::{Access, MapAlloc, MapBuilder, Session, SessionBy};
use crate::tests::{self, Info, tracing_init};

type UnixAlloc = MapAlloc<AddrSpec, FdBackend>;
type UnixSession<H, const N: usize> = Session<H, N, AddrSpec, FdBackend>;

fn mock_alloc(name: &str, size: usize) -> UnixAlloc {
    let fd = UnixFd::memfd(name, size, false).expect("should create");
    let builder = MapBuilder::fd();
    builder
        .shared(size, Access::WRITE | Access::READ, fd)
        .unwrap()
}

#[test]
fn alloc_content() {
    // 2 kb
    const BYTES_SIZE: usize = 20;
    const ALLOC_NUM: usize = 200;
    const NUM: usize = 500;

    const NAME: &str = "alloc";
    const SIZE: usize = (BYTES_SIZE * ALLOC_NUM).max(10000).next_power_of_two();

    let a = mock_alloc(NAME, SIZE);

    tests::alloc_content::<BYTES_SIZE, ALLOC_NUM, NUM>(a);
}