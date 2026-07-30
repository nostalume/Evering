use crate::os::unix::UnixFd;

use crate::perlude::talc::{Access, Request};
use crate::tests;
use crate::{RegionAdmission, RegionId};

type UnixAlloc = crate::talc::MapTalc;
fn mock_alloc(name: &str, size: usize) -> UnixAlloc {
    let fd = UnixFd::memfd(name, size, false).expect("should create");
    crate::MapLayout::map(
        fd,
        Request::new(size, Access::WRITE | Access::READ),
        RegionAdmission::Create(RegionId::new(0x5441_4c43, size as u64)),
    )
    .unwrap()
    .try_into()
    .unwrap()
}

#[test]
fn layout_order_is_rejected_at_the_first_unexpected_record() {
    const SIZE: usize = 1 << 19;
    const REGION: RegionId = RegionId::new(0x004f_5244_4552, 1);
    let fd = UnixFd::memfd("layout-order", SIZE, false).expect("create shared memory");
    let created = crate::perlude::talc::SessionBy::<()>::create(
        fd.dup().expect("duplicate shared-memory handle"),
        Request::new(SIZE, Access::WRITE | Access::READ),
        REGION,
    )
    .expect("record canonical layout order");
    drop(created);

    let mut layout = crate::MapLayout::map(
        fd,
        Request::new(SIZE, Access::WRITE | Access::READ),
        RegionAdmission::Expect(REGION),
    )
    .expect("map region");
    let conf = crate::talc::Config::new(SIZE);
    let reserve = layout
        .reserve::<crate::talc::Header>()
        .expect("reserve allocator");
    let conf = conf.with_bound(reserve.remaining_after());

    let error = reserve
        .commit(conf)
        .err()
        .expect("reversed layout order must reject");

    assert!(matches!(
        error,
        crate::MapError::LayoutMismatch(crate::LayoutField::Magic)
    ));
    assert!(matches!(
        layout.push::<crate::dir::Header>(()),
        Err(crate::MapError::PoisonedComposition)
    ));
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

#[test]
fn real_mapping_reuse_and_tokens() {
    const NAME: &str = "talc-reuse";
    const SIZE: usize = 1 << 19;

    tests::alloc_lines::<2048, 200, 5>(mock_alloc(NAME, SIZE));
    tests::pbox_token::<100, 5>(mock_alloc("talc-token", SIZE));
    tests::pbox_rand::<100, 1>(mock_alloc("talc-rand", SIZE));
}
