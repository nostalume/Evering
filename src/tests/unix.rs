#![cfg(feature = "map")]
#![cfg(test)]

use crate::mem::{Access, MapLayout, MapView, Request};
use crate::os::unix::UnixFd;
use crate::tests;
use crate::{RegionAdmission, RegionId};

mod recovery;
mod talc;

type UnixMapView = MapView;

fn mock_view(name: &str, size: usize) -> UnixMapView {
    let fd = UnixFd::memfd(name, size, false).expect("should create");
    MapLayout::map(
        fd,
        Request::new(size, Access::WRITE | Access::READ),
        RegionAdmission::Create(RegionId::new(0x4556_4552, size as u64)),
    )
    .unwrap()
    .try_into()
    .unwrap()
}

#[test]
fn area_init() {
    const SIZE: usize = 2000;
    const NAME: &str = "area";

    let area = mock_view(NAME, SIZE);
    tests::area_init(area);
}
