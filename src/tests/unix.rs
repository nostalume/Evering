#![cfg(feature = "map")]
#![cfg(test)]

use crate::mem::{Access, Build, Request, Source};
use crate::os::unix::UnixFd;
use crate::schema::{RegionAdmission, RegionId};
use crate::tests;

mod recovery;
mod talc;

fn mock_view(name: &str, size: usize) -> Build {
    let fd = UnixFd::memfd(name, size, false).expect("should create");
    let map = fd
        .map(Request::new(size, Access::WRITE | Access::READ))
        .unwrap();
    Build::new(
        map,
        RegionAdmission::Create(RegionId::new(0x4556_4552, size as u64)),
    )
    .unwrap()
}

#[test]
fn area_init() {
    const SIZE: usize = 2000;
    const NAME: &str = "area";

    let area = mock_view(NAME, SIZE);
    tests::area_init(area);
}
