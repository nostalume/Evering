//! The Unix arm of the in-crate suite: `mock_view` hands the platform-neutral bodies in `crate::tests` a
//! real memfd mapping instead of the heap one, and `recovery` and `talc` below are the cases that only
//! mean something once the bytes are shared-memory. Nothing here owns state a test may not rebuild, and
//! no fixture reaches a second process — cross-process cases live in `tests/`.
//!
//! Because the mapping is made through `Source::map`, every byte these tests see has been through the
//! backend's reserve-then-place path, so an assertion that hard-codes an address or a residue instead of
//! asking `crate::mem::REGION_ALIGN` proves the host, not the library. The whole file is also gated on
//! `map`, so a change to a shared body can be green on the mock allocator and never once run here.

#![cfg(feature = "map")]
#![cfg(test)]

use crate::mem::{Access, Build, Request, Source};
use crate::os::unix::UnixFd;
use crate::place::{RegionAdmission, RegionId};
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
