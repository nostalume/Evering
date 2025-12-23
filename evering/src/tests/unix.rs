#![cfg(feature = "unix")]
#![cfg(test)]

use crate::mem::{Access, MapBuilder, MapView};
use crate::os::FdBackend;
use crate::os::unix::{AddrSpec, UnixFd};
use crate::tests;

mod arena;
mod talc;

type UnixMapView = MapView<AddrSpec, FdBackend>;

fn mock_view(name: &str, size: usize) -> UnixMapView {
    let fd = UnixFd::memfd(name, size, false).expect("should create");
    let builder = MapBuilder::fd();
    builder
        .shared(size, Access::WRITE | Access::READ, fd)
        .unwrap()
}

#[test]
fn area_init() {
    const SIZE: usize = 2000;
    const NAME: &str = "area";

    let area = mock_view(NAME, SIZE);
    tests::area_init(area);
}
